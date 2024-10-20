// Copyright (c) 2024-present, fjall-rs
// This source code is licensed under both the Apache 2.0 and MIT License
// (found in the LICENSE-* files in the repository)

use crate::{gc::GarbageCollection, PartitionHandle};
use lsm_tree::{GcReport, UserValue};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

/// Access to a partition of a transactional keyspace
#[derive(Clone)]
pub struct TransactionalPartitionHandle {
    pub(crate) inner: PartitionHandle,
    pub(crate) tx_lock: Arc<Mutex<()>>,
}

impl GarbageCollection for TransactionalPartitionHandle {
    fn gc_scan(&self) -> crate::Result<GcReport> {
        crate::gc::GarbageCollector::scan(self.inner())
    }

    fn gc_with_space_amp_target(&self, factor: f32) -> crate::Result<u64> {
        let _lock = self.tx_lock.lock().expect("lock is poisoned");
        crate::gc::GarbageCollector::with_space_amp_target(self.inner(), factor)
    }

    fn gc_with_staleness_threshold(&self, threshold: f32) -> crate::Result<u64> {
        let _lock = self.tx_lock.lock().expect("lock is poisoned");
        crate::gc::GarbageCollector::with_staleness_threshold(self.inner(), threshold)
    }

    fn gc_drop_stale_segments(&self) -> crate::Result<u64> {
        crate::gc::GarbageCollector::drop_stale_segments(self.inner())
    }
}

impl TransactionalPartitionHandle {
    /// Returns the underlying LSM-tree's path
    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.inner.path().into()
    }

    /// Removes an item and returns its value if it existed.
    ///
    /// The operation will run wrapped in a transaction.
    ///
    /// ```
    /// # use fjall::{Config, Keyspace, PartitionCreateOptions};
    /// # use std::sync::Arc;
    /// #
    /// # let folder = tempfile::tempdir()?;
    /// # let keyspace = Config::new(folder).open_transactional()?;
    /// # let partition = keyspace.open_partition("default", PartitionCreateOptions::default())?;
    /// partition.insert("a", "abc")?;
    ///
    /// let taken = partition.take("a")?.unwrap();
    /// assert_eq!(b"abc", &*taken);
    ///
    /// let item = partition.get("a")?;
    /// assert!(item.is_none());
    /// #
    /// # Ok::<(), fjall::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Will return `Err` if an IO error occurs.
    pub fn take<K: AsRef<[u8]>>(&self, key: K) -> crate::Result<Option<UserValue>> {
        self.fetch_update(key, |_| None)
    }

    /// Atomically updates an item and returns the previous value.
    ///
    /// Returning `None` removes the item if it existed before.
    ///
    /// The operation will run wrapped in a transaction.
    ///
    /// # Examples
    ///
    /// ```
    /// # use fjall::{Config, Keyspace, Slice, PartitionCreateOptions};
    /// # use std::sync::Arc;
    /// #
    /// # let folder = tempfile::tempdir()?;
    /// # let keyspace = Config::new(folder).open_transactional()?;
    /// # let partition = keyspace.open_partition("default", PartitionCreateOptions::default())?;
    /// partition.insert("a", "abc")?;
    ///
    /// let prev = partition.fetch_update("a", |_| Some(Slice::from(*b"def")))?.unwrap();
    /// assert_eq!(b"abc", &*prev);
    ///
    /// let item = partition.get("a")?;
    /// assert_eq!(Some("def".as_bytes().into()), item);
    /// #
    /// # Ok::<(), fjall::Error>(())
    /// ```
    ///
    /// ```
    /// # use fjall::{Config, Keyspace, PartitionCreateOptions};
    /// # use std::sync::Arc;
    /// #
    /// # let folder = tempfile::tempdir()?;
    /// # let keyspace = Config::new(folder).open_transactional()?;
    /// # let partition = keyspace.open_partition("default", PartitionCreateOptions::default())?;
    /// partition.insert("a", "abc")?;
    ///
    /// let prev = partition.fetch_update("a", |_| None)?.unwrap();
    /// assert_eq!(b"abc", &*prev);
    ///
    /// let item = partition.get("a")?;
    /// assert!(item.is_none());
    /// #
    /// # Ok::<(), fjall::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Will return `Err` if an IO error occurs.
    pub fn fetch_update<K: AsRef<[u8]>, F: Fn(Option<&UserValue>) -> Option<UserValue>>(
        &self,
        key: K,
        f: F,
    ) -> crate::Result<Option<UserValue>> {
        let _lock = self.tx_lock.lock().expect("lock is poisoned");

        let prev = self.inner.get(&key)?;
        let updated = f(prev.as_ref());

        if let Some(value) = updated {
            self.inner.insert(&key, value)?;
        } else if prev.is_some() {
            self.inner.remove(&key)?;
        }

        Ok(prev)
    }

    /// Atomically updates an item and returns the new value.
    ///
    /// Returning `None` removes the item if it existed before.
    ///
    /// The operation will run wrapped in a transaction.
    ///
    /// # Examples
    ///
    /// ```
    /// # use fjall::{Config, Keyspace, Slice, PartitionCreateOptions};
    /// # use std::sync::Arc;
    /// #
    /// # let folder = tempfile::tempdir()?;
    /// # let keyspace = Config::new(folder).open_transactional()?;
    /// # let partition = keyspace.open_partition("default", PartitionCreateOptions::default())?;
    /// partition.insert("a", "abc")?;
    ///
    /// let updated = partition.update_fetch("a", |_| Some(Slice::from(*b"def")))?.unwrap();
    /// assert_eq!(b"def", &*updated);
    ///
    /// let item = partition.get("a")?;
    /// assert_eq!(Some("def".as_bytes().into()), item);
    /// #
    /// # Ok::<(), fjall::Error>(())
    /// ```
    ///
    /// ```
    /// # use fjall::{Config, Keyspace, PartitionCreateOptions};
    /// # use std::sync::Arc;
    /// #
    /// # let folder = tempfile::tempdir()?;
    /// # let keyspace = Config::new(folder).open_transactional()?;
    /// # let partition = keyspace.open_partition("default", PartitionCreateOptions::default())?;
    /// partition.insert("a", "abc")?;
    ///
    /// let updated = partition.update_fetch("a", |_| None)?;
    /// assert!(updated.is_none());
    ///
    /// let item = partition.get("a")?;
    /// assert!(item.is_none());
    /// #
    /// # Ok::<(), fjall::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Will return `Err` if an IO error occurs.
    pub fn update_fetch<K: AsRef<[u8]>, F: Fn(Option<&UserValue>) -> Option<UserValue>>(
        &self,
        key: K,
        f: F,
    ) -> crate::Result<Option<UserValue>> {
        let _lock = self.tx_lock.lock().expect("lock is poisoned");

        let prev = self.inner.get(&key)?;
        let updated = f(prev.as_ref());

        if let Some(value) = &updated {
            self.inner.insert(&key, value)?;
        } else if prev.is_some() {
            self.inner.remove(&key)?;
        }

        Ok(updated)
    }

    /// Inserts a key-value pair into the partition.
    ///
    /// Keys may be up to 65536 bytes long, values up to 2^32 bytes.
    /// Shorter keys and values result in better performance.
    ///
    /// If the key already exists, the item will be overwritten.
    ///
    /// The operation will run wrapped in a transaction.
    ///
    /// # Examples
    ///
    /// ```
    /// # use fjall::{Config, Keyspace, PartitionCreateOptions};
    /// #
    /// # let folder = tempfile::tempdir()?;
    /// # let keyspace = Config::new(folder).open_transactional()?;
    /// # let partition = keyspace.open_partition("default", PartitionCreateOptions::default())?;
    /// partition.insert("a", "abc")?;
    ///
    /// assert!(!keyspace.read_tx().is_empty(&partition)?);
    /// #
    /// # Ok::<(), fjall::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Will return `Err` if an IO error occurs.
    pub fn insert<K: AsRef<[u8]>, V: AsRef<[u8]>>(&self, key: K, value: V) -> crate::Result<()> {
        let _lock = self.tx_lock.lock().expect("lock is poisoned");
        self.inner.insert(key, value)
    }

    /// Removes an item from the partition.
    ///
    /// The key may be up to 65536 bytes long.
    /// Shorter keys result in better performance.
    ///
    /// The operation will run wrapped in a transaction.
    ///
    /// # Examples
    ///
    /// ```
    /// # use fjall::{Config, Keyspace, PartitionCreateOptions};
    /// #
    /// # let folder = tempfile::tempdir()?;
    /// # let keyspace = Config::new(folder).open_transactional()?;
    /// # let partition = keyspace.open_partition("default", PartitionCreateOptions::default())?;
    /// partition.insert("a", "abc")?;
    /// assert!(!keyspace.read_tx().is_empty(&partition)?);
    ///
    /// partition.remove("a")?;
    /// assert!(keyspace.read_tx().is_empty(&partition)?);
    /// #
    /// # Ok::<(), fjall::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Will return `Err` if an IO error occurs.
    pub fn remove<K: AsRef<[u8]>>(&self, key: K) -> crate::Result<()> {
        let _lock = self.tx_lock.lock().expect("lock is poisoned");
        self.inner.remove(key)
    }

    /// Retrieves an item from the partition.
    ///
    /// The operation will run wrapped in a read snapshot.
    ///
    /// # Examples
    ///
    /// ```
    /// # use fjall::{Config, Keyspace, PartitionCreateOptions};
    /// #
    /// # let folder = tempfile::tempdir()?;
    /// # let keyspace = Config::new(folder).open_transactional()?;
    /// # let partition = keyspace.open_partition("default", PartitionCreateOptions::default())?;
    /// partition.insert("a", "my_value")?;
    ///
    /// let item = partition.get("a")?;
    /// assert_eq!(Some("my_value".as_bytes().into()), item);
    /// #
    /// # Ok::<(), fjall::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Will return `Err` if an IO error occurs.
    pub fn get<K: AsRef<[u8]>>(&self, key: K) -> crate::Result<Option<lsm_tree::UserValue>> {
        self.inner.get(key)
    }

    /// Returns `true` if the partition contains the specified key.
    ///
    /// The operation will run wrapped in a read snapshot.
    ///
    /// # Examples
    ///
    /// ```
    /// # use fjall::{Config, Keyspace, PartitionCreateOptions};
    /// #
    /// # let folder = tempfile::tempdir()?;
    /// # let keyspace = Config::new(folder).open_transactional()?;
    /// # let partition = keyspace.open_partition("default", PartitionCreateOptions::default())?;
    /// partition.insert("a", "my_value")?;
    ///
    /// assert!(partition.contains_key("a")?);
    /// assert!(!partition.contains_key("b")?);
    /// #
    /// # Ok::<(), fjall::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Will return `Err` if an IO error occurs.
    pub fn contains_key<K: AsRef<[u8]>>(&self, key: K) -> crate::Result<bool> {
        self.inner.contains_key(key)
    }

    /// Allows access to the inner partition handle, allowing to
    /// escape from the transactional context.
    #[doc(hidden)]
    #[must_use]
    pub fn inner(&self) -> &PartitionHandle {
        &self.inner
    }
}
