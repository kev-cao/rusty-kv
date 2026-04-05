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

struct MemTableImpl<W: Wal> {
    id: u64,
    btree: BTree<Vec<u8>, Vec<u8>>,
    wal: W,
    max_size: usize,
    current_size: usize,
}

/// The MemTableImpl is an in-memory sorted structure that provides
/// high-performance writes and reads. It is backed by a Write-Ahead Log (WAL)
/// to ensure durability in the event of a crash.
impl<W: Wal> MemTableImpl<W> {
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
    pub fn iter(&self) -> impl SeekableIterator<Vec<u8>, Item = (&Vec<u8>, &Vec<u8>)> {
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

pub(crate) type MemTableProd = MemTableImpl<wal::FileWal>;
pub(crate) type MemTableDev = MemTableImpl<wal::MemWal>;

impl MemTableImpl<wal::FileWal> {
    /// Creates a new MemTable with a given ID and size threshold. The WAL is
    /// written to the directory specified by path.
    pub fn new(id: u64, path: &Path, max_size: usize) -> Result<Self, MemTableError> {
        let wal_path = path.join(format!("memtable_{}.wal", id));
        Ok(Self {
            id,
            // TODO (kev-cao): Should set up configurations for WAL mode and
            // BTree degree.
            wal: wal::FileWal::new(&wal_path, wal::SyncMode::Async)?,
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
}

impl MemTableImpl<wal::MemWal> {
    /// Creates a new in-memory MemTable with a given ID and size threshold.
    pub fn new(max_size: usize) -> Self {
        Self {
            id: 0,
            wal: wal::MemWal::new(),
            btree: BTree::new(16),
            current_size: 0,
            max_size,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::macros::seeded_rng;
    use rand::Rng;

    const MAX_TEST_KEY_LEN: usize = u16::MAX as usize;
    const MAX_TEST_VAL_LEN: usize = u16::MAX as usize;

    #[test]
    fn put_get_roundtrip() {
        let (mut rng, _) = seeded_rng!();
        let mut memtable = MemTableDev::new(1024);

        let mut appended: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        for _ in 0..100 {
            let key_len = rng.random_range(0..=MAX_TEST_KEY_LEN);
            let val_len = rng.random_range(0..=MAX_TEST_VAL_LEN);
            let key = generate_rand_bytes(key_len, &mut rng);
            let value = generate_rand_bytes(val_len, &mut rng);

            appended.push((key.clone(), value.clone()));
            memtable
                .put(key, value)
                .expect("failed to put into memtable");
        }

        for (k, v) in &appended {
            assert_eq!(
                memtable
                    .get(k.as_slice())
                    .expect("failed to get known key in memtable"),
                v,
                "memtable.get did not match expected value"
            );
        }

        appended.sort_by(|a, b| a.0.cmp(&b.0));

        for (i, (k, v)) in memtable.iter().enumerate() {
            assert_eq!(
                k, &appended[i].0,
                "memtable.iter did not return keys in sorted order"
            );
            assert_eq!(
                v, &appended[i].1,
                "memtable.iter did not return expected value"
            );
        }
    }

    #[test]
    fn is_full() {
        let mut memtable = MemTableDev::new(10);
        assert!(!memtable.is_full(), "memtable should not be full initially");

        memtable
            .put(b"key".to_vec(), b"value".to_vec())
            .expect("failed to put into memtable");

        assert!(
            !memtable.is_full(),
            "memtable should not be full after put with size less than max_size",
        );

        memtable
            .put(b"key2".to_vec(), b"value2".to_vec())
            .expect("failed to put into memtable");

        assert!(
            memtable.is_full(),
            "memtable should be full after put with size equal to or greater than max_size",
        )
    }

    fn generate_rand_bytes(n: usize, mut rng: impl Rng) -> Vec<u8> {
        let mut data = vec![0u8; n];
        rng.fill_bytes(&mut data[..]);
        data
    }
}
