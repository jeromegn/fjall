// Copyright (c) 2024-present, fjall-rs
// This source code is licensed under both the Apache 2.0 and MIT License
// (found in the LICENSE-* files in the repository)

use parking_lot::FairMutex;

#[cfg(feature = "ssi_tx")]
use super::{conflict_manager::ConflictChecker, oracle::Oracle};
use super::{read_tx::ReadTransaction, write_tx::WriteTransaction};
use crate::{
    batch::PartitionKey, snapshot_nonce::SnapshotNonce, Config, Keyspace, PartitionCreateOptions,
    PersistMode, TxPartitionHandle,
};
use std::sync::{Arc, Mutex};

/// Transaction keyspace
#[derive(Clone)]
#[allow(clippy::module_name_repetitions)]
pub struct TransactionalKeyspace {
    pub(crate) inner: Keyspace,
    #[cfg(feature = "ssi_tx")]
    pub(super) orc: Arc<Oracle>,
    #[cfg(feature = "single_writer_tx")]
    lock: Arc<Mutex<()>>,
}

/// Alias for [`TransactionalKeyspace`]
#[allow(clippy::module_name_repetitions)]
pub type TxKeyspace = TransactionalKeyspace;

impl TxKeyspace {
    /// Starts a new writeable transaction.
    #[cfg(feature = "single_writer_tx")]
    #[must_use]
    pub fn write_tx(&self) -> WriteTransaction {
        let guard = self.lock.lock().expect("poisoned tx lock");
        let instant = self.inner.instant();

        let mut write_tx = WriteTransaction::new(
            self.clone(),
            SnapshotNonce::new(instant, self.inner.snapshot_tracker.clone()),
            guard,
        );

        if !self.inner.config.manual_journal_persist {
            write_tx = write_tx.durability(Some(PersistMode::Buffer));
        }

        write_tx
    }

    /// Starts a new writeable transaction.
    /// # Errors
    ///
    /// Will return `Err` if creating a SSI transaction fails
    #[cfg(feature = "ssi_tx")]
    pub fn write_tx(&self) -> crate::Result<WriteTransaction> {
        let instant = {
            // acquire a lock here to prevent geting a stale snapshot seqno
            // this will drain the queue of transaction commits before this time
            let _guard = self.orc.write_serialize_lock.lock();
            self.inner.instant()
        };

        let mut write_tx = WriteTransaction::new(
            self.clone(),
            SnapshotNonce::new(instant, self.inner.snapshot_tracker.clone()),
        );

        if !self.inner.config.manual_journal_persist {
            write_tx = write_tx.durability(Some(PersistMode::Buffer));
        }

        Ok(write_tx)
    }

    /// Starts a new read-only transaction.
    #[must_use]
    pub fn read_tx(&self) -> ReadTransaction {
        ReadTransaction::new(SnapshotNonce::new(
            self.inner.instant(),
            self.inner.snapshot_tracker.clone(),
        ))
    }

    /// Flushes the active journal to OS buffers. The durability depends on the [`PersistMode`]
    /// used.
    ///
    /// Persisting only affects durability, NOT consistency! Even without flushing
    /// data is crash-safe.
    ///
    /// # Examples
    ///
    /// ```
    /// # use fjall::{Config, PersistMode, Keyspace, PartitionCreateOptions};
    /// # let folder = tempfile::tempdir()?;
    /// let keyspace = Config::new(folder).open_transactional()?;
    /// let items = keyspace.open_partition("my_items", PartitionCreateOptions::default())?;
    ///
    /// items.insert("a", "hello")?;
    ///
    /// keyspace.persist(PersistMode::SyncAll)?;
    /// #
    /// # Ok::<_, fjall::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns error, if an IO error occurred.
    pub fn persist(&self, mode: PersistMode) -> crate::Result<()> {
        self.inner.persist(mode)
    }

    /// Creates or opens a keyspace partition.
    ///
    /// # Errors
    ///
    /// Returns error, if an IO error occurred.
    ///
    /// # Panics
    ///
    /// Panics if the partition name includes characters other than: a-z A-Z 0-9 _ -
    pub fn open_partition(
        &self,
        name: &str,
        create_options: PartitionCreateOptions,
    ) -> crate::Result<TxPartitionHandle> {
        let partition = self.inner.open_partition(name, create_options)?;

        Ok(TxPartitionHandle {
            inner: partition,
            keyspace: self.clone(),
        })
    }

    /// Returns the amount of partitions
    #[must_use]
    pub fn partition_count(&self) -> usize {
        self.inner.partition_count()
    }

    /// Gets a list of all partition names in the keyspace
    #[must_use]
    pub fn list_partitions(&self) -> Vec<PartitionKey> {
        self.inner.list_partitions()
    }

    /// Returns `true` if the partition with the given name exists.
    #[must_use]
    pub fn partition_exists(&self, name: &str) -> bool {
        self.inner.partition_exists(name)
    }

    /// Destroys the partition, removing all data associated with it.
    ///
    /// # Errors
    ///
    /// Will return `Err` if an IO error occurs.
    pub fn delete_partition(&self, handle: TxPartitionHandle) -> crate::Result<()> {
        self.inner.delete_partition(handle.inner)
    }

    /// Returns the current write buffer size (active + sealed memtables).
    #[must_use]
    pub fn write_buffer_size(&self) -> u64 {
        self.inner.write_buffer_size()
    }

    /// Returns the amount of journals on disk.
    #[must_use]
    pub fn journal_count(&self) -> usize {
        self.inner.journal_count()
    }

    /// Returns the disk space usage of the entire keyspace.
    #[must_use]
    pub fn disk_space(&self) -> u64 {
        self.inner.disk_space()
    }

    /// Opens a keyspace in the given directory.
    ///
    /// # Errors
    ///
    /// Returns error, if an IO error occurred.
    pub fn open(config: Config) -> crate::Result<Self> {
        let inner = Keyspace::create_or_recover(config)?;
        inner.start_background_threads();

        Ok(Self {
            #[cfg(feature = "ssi_tx")]
            orc: Arc::new(Oracle {
                write_serialize_lock: FairMutex::default(),
                seqno: inner.seqno.clone(),
                snapshot_tracker: inner.snapshot_tracker.clone(),
            }),
            inner,
            #[cfg(feature = "single_writer_tx")]
            lock: Default::default(),
        })
    }
}
