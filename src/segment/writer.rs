use super::block::ValueBlock;
use crate::{
    segment::index::writer::Writer as IndexWriter, serde::Serializable, value::SeqNo, Value,
};
use lz4_flex::compress_prepend_size;
use std::{
    fs::File,
    io::{BufWriter, Write},
    path::PathBuf,
};

pub struct Writer {
    opts: Options,

    block_writer: BufWriter<File>,
    index_writer: IndexWriter,
    chunk: ValueBlock,

    block_count: usize,
    written_item_count: usize,
    file_pos: u64,
    uncompressed_size: u64,

    first_key: Option<Vec<u8>>,
    last_key: Option<Vec<u8>>,
    tombstone_count: usize,
    chunk_size: usize,

    lowest_seqno: SeqNo,
    highest_seqno: SeqNo,
}

pub struct Options {
    path: PathBuf,
    evict_tombstones: bool,
    block_size: u32,
    index_block_size: u32,
}

impl Writer {
    pub fn new(opts: Options) -> std::io::Result<Self> {
        std::fs::create_dir_all(&opts.path)?;

        let block_writer = File::create(opts.path.join("blocks"))?;
        let mut block_writer = BufWriter::with_capacity(512_000, block_writer);

        let mut index_writer = IndexWriter::new(&opts.path, opts.index_block_size)?;

        let mut chunk = ValueBlock {
            items: Vec::with_capacity(1_000),
            crc: 0,
        };

        let mut block_count: usize = 0;
        let mut written_item_count = 0;
        let mut file_pos: u64 = 0;
        let mut uncompressed_size: u64 = 0;

        Ok(Self {
            opts,

            block_writer,
            index_writer,
            chunk,

            block_count,
            written_item_count,
            file_pos,
            uncompressed_size,

            first_key: None,
            last_key: None,
            chunk_size: 0,
            tombstone_count: 0,

            lowest_seqno: SeqNo::MAX,
            highest_seqno: 0,
        })
    }

    fn write_block(&mut self) -> std::io::Result<()> {
        debug_assert!(!self.chunk.items.is_empty());

        let uncompressed_chunk_size = self
            .chunk
            .items
            .iter()
            .map(|item| item.size() as u64)
            .sum::<u64>();

        self.uncompressed_size += uncompressed_chunk_size;

        // Serialize block
        let mut bytes = Vec::with_capacity(u16::MAX.into());
        self.chunk.crc = ValueBlock::create_crc(&self.chunk.items);
        self.chunk.serialize(&mut bytes).unwrap();

        // Compress using LZ4
        let bytes = compress_prepend_size(&bytes);

        // Write to file
        self.block_writer.write_all(&bytes)?;

        // NOTE: Blocks are never bigger than 4 GB anyway,
        // so it's fine to just truncate it
        #[allow(clippy::cast_possible_truncation)]
        let bytes_written = bytes.len() as u32;

        // Expect is fine, because the chunk is not empty
        let first = self.chunk.items.first().expect("Chunk should not be empty");

        self.index_writer
            .register_block(first.key.clone(), self.file_pos, bytes_written)?;

        // TODO:  Add to bloom filter

        // Adjust metadata
        log::trace!(
            "Written data block @ {} ({} bytes, uncompressed: {} bytes)",
            self.file_pos,
            bytes_written,
            uncompressed_chunk_size
        );

        self.file_pos += u64::from(bytes_written);
        self.written_item_count += self.chunk.items.len();
        self.block_count += 1;
        self.chunk.items.clear();

        Ok(())
    }

    pub fn write(&mut self, item: Value) -> std::io::Result<()> {
        if item.is_tombstone {
            if self.opts.evict_tombstones {
                return Ok(());
            }

            self.tombstone_count += 1;
        }

        let item_key = item.key.clone();
        let seqno = item.seqno;

        self.chunk_size += item.size();
        self.chunk.items.push(item);

        if self.chunk_size >= self.opts.block_size as usize {
            self.write_block()?;
            self.chunk_size = 0;
        }

        if self.first_key.is_none() {
            self.first_key = Some(item_key.clone());
        }
        self.last_key = Some(item_key);

        if self.lowest_seqno > seqno {
            self.lowest_seqno = seqno;
        }

        if self.highest_seqno < seqno {
            self.highest_seqno = seqno;
        }

        Ok(())
    }

    pub fn finalize(&mut self) -> std::io::Result<()> {
        if !self.chunk.items.is_empty() {
            self.write_block()?;
        }

        // TODO: bloom etc

        self.index_writer.finalize()?;

        self.block_writer.flush()?;
        self.block_writer.get_mut().sync_all()?;

        log::debug!(
            "Written {} items in {} blocks into new segment file, written {} MB",
            self.written_item_count,
            self.block_count,
            self.file_pos / 1024 / 1024
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{segment::index::MetaIndex, Value};
    use std::path::Path;
    use std::sync::Arc;
    use test_log::test;

    #[test]
    fn test_write() {
        // TODO: tempfile

        // TODO: remove
        if Path::new(".peter").exists() {
            std::fs::remove_dir_all(".peter").unwrap();
        }

        let NUM_ITEMS = 8_000_000;

        let mut items = (0u64..NUM_ITEMS)
            .map(|i| Value::new(i.to_be_bytes(), nanoid::nanoid!(), false, 1000 + i))
            .collect::<Vec<_>>();

        items.sort_by(|a, b| a.key.cmp(&b.key));

        let mut writer = Writer::new(Options {
            path: ".peter".into(),
            evict_tombstones: false,
            block_size: 4096,
            index_block_size: 4096,
        })
        .unwrap();

        for item in items {
            writer.write(item).unwrap();
        }

        writer.finalize().unwrap();

        let meta_index = Arc::new(MetaIndex::from_file(".peter").unwrap());

        for NUM_THREADS in [1, 1, 2, 4] {
            let start = std::time::Instant::now();
            eprintln!("getting 400k items with {NUM_THREADS} threads");

            let threads = (0..NUM_THREADS)
                .map(|thread_no| {
                    let meta_index = meta_index.clone();

                    std::thread::spawn(move || {
                        let item_count = NUM_ITEMS / NUM_THREADS;
                        let start = thread_no * item_count;
                        let range = start..start + item_count;

                        for key in range.map(u64::to_be_bytes) {
                            let item = meta_index.get_latest(&key);

                            match item {
                                Some(item) => {
                                    assert_eq!(key, &*item.key);
                                }
                                None => panic!("item should exist"),
                            }
                        }
                    })
                })
                .collect::<Vec<_>>();

            for thread in threads {
                thread.join().unwrap();
            }

            let elapsed = start.elapsed();
            let nanos = elapsed.as_nanos();
            let nanos_per_item = nanos / u128::from(NUM_ITEMS);
            let reads_per_second = (std::time::Duration::from_secs(1)).as_nanos() / nanos_per_item;

            eprintln!(
                "done in {:?}s, {}ns per item - {} RPS",
                elapsed.as_secs_f64(),
                nanos_per_item,
                reads_per_second
            );
        }
    }
}
