use std::borrow::Borrow;
use std::path::Path;

use crate::storage::collections::btree::{BTree, BTreeIter};

pub mod wal;

/// The MemTableImpl is an in-memory sorted structure that provides
/// high-performance writes and reads. It is backed by a Write-Ahead Log (WAL)
/// to ensure durability in the event of a crash.
pub struct MemTableImpl<W: wal::Wal> {
    id: u64,
    btree: BTree<Vec<u8>, Vec<u8>>,
    wal: W,
    max_size: usize,
    current_size: usize,
}

impl<W: wal::Wal> MemTableImpl<W> {
    /// Creates a new MemTable with a specific ID and size threshold.
    /// This should also initialize a new WAL file on disk named after the ID.
    pub fn create(id: u64, path: &Path, max_size: usize) -> Result<Self, ()> {
        todo!()
    }

    /// Recovers a MemTable from an existing WAL file.
    /// This is used during database startup to rebuild the state after a crash.
    pub fn recover(id: u64, path: &Path) -> Result<Self, ()> {
        todo!()
    }

    /// Inserts a key-value pair.
    /// Requirement: The entry MUST be successfully appended to the WAL
    /// before it is inserted into the BTree.
    pub fn put(&mut self, key: Vec<u8>, value: Vec<u8>) -> Result<(), ()> {
        todo!()
    }

    /// Retrieves a value from the BTree.
    pub fn get<K>(&self, key: K) -> Option<&[u8]>
    where
        K: Ord + Borrow<[u8]>,
    {
        todo!()
    }

    /// Returns an iterator over the contents of the MemTable.
    pub fn iter(&self) -> BTreeIter<Vec<u8>, Vec<u8>> {
        todo!()
    }

    /// Checks if the MemTable has exceeded its `max_size`.
    /// If true, the database will likely "freeze" this table and start a new one.
    pub fn is_full(&self) -> bool {
        todo!()
    }

    /// Closes the MemTable, ensuring the WAL is properly synced to disk.
    pub fn sync(&mut self) -> Result<(), ()> {
        todo!()
    }
}
