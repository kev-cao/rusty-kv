pub mod wal;

use std::borrow::Borrow;
use std::path::Path;
use wal::Wal;

use crate::storage::collections::{KVStore, SeekableIterator, btree::BTree};

#[derive(thiserror::Error, Debug)]
pub enum MemTableError {
    #[error("WAL operation failed")]
    WalOpError(#[from] wal::WalError),
}

/// The MemTableProd is an in-memory sorted structure that provides
/// high-performance writes and reads. It is backed by a Write-Ahead Log (WAL)
/// to ensure durability in the event of a crash.
pub struct MemTableProd {
    id: u64,
    btree: BTree<Vec<u8>, Vec<u8>>,
    wal: wal::FileWal,
    max_size: usize,
    current_size: usize,
}

impl MemTableProd {
    /// Creates a new MemTable with a given ID and size threshold. The WAL is
    /// written to the directory specified by path.
    pub fn create(id: u64, path: &Path, max_size: usize) -> Result<Self, MemTableError> {
        let wal_path = path.join(format!("memtable_{}.wal", id));
        Ok(Self {
            id,
            // TODO (kev-cao): Should set up configurations for WAL mode and
            // BTree degree.
            wal: wal::FileWal::create(&wal_path, wal::SyncMode::Async)?,
            btree: BTree::new(16),
            current_size: 0,
            max_size,
        })
    }

    /// Recovers a MemTable from an existing WAL file.
    pub fn recover(id: u64, path: &Path, max_size: usize) -> Result<Self, MemTableError> {
        let wal_path = path.join(format!("memtable_{}.wal", id));
        let kvs = wal::FileWal::recover(&wal_path)?;
        // TODO (kev-cao): Configurations for BTree degree.
        let mut btree: BTree<Vec<u8>, Vec<u8>> = BTree::new(16);
        let mut size: usize = 0;
        for (k, v) in kvs {
            size += k.len() + v.len();
            btree.put(k, v);
        }
        Ok(Self {
            id,
            // TODO (kev-cao): Same here.
            wal: wal::FileWal::open(&wal_path, wal::SyncMode::Async)?,
            btree,
            current_size: size,
            max_size,
        })
    }

    /// Inserts a key-value pair.
    pub fn put(&mut self, key: Vec<u8>, value: Vec<u8>) -> Result<(), MemTableError> {
        self.wal.append(&key, &value)?;
        self.current_size += key.len() + value.len();
        self.btree.put(key, value);
        Ok(())
    }

    /// Retrieves a value from the BTree.
    pub fn get<K>(&self, key: K) -> Option<&[u8]>
    where
        K: Borrow<[u8]>,
    {
        self.btree.get(key.borrow()).map(|v| v.as_slice())
    }

    /// Returns an iterator over the contents of the MemTable.
    pub fn iter(&self) -> impl SeekableIterator<Vec<u8>> {
        self.btree.iter()
    }

    /// Checks if the MemTable has exceeded its `max_size`.
    pub fn is_full(&self) -> bool {
        self.current_size >= self.max_size
    }

    /// Syncs the MemTable's WAL to disk.
    pub fn sync(&mut self) -> Result<(), MemTableError> {
        self.wal.sync().map_err(Into::into)
    }
}
