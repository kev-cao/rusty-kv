pub(crate) mod wal;

use snafu::prelude::*;
use std::path::Path;
use wal::Wal;

use crate::storage::collections::{KVIter, btree::BTree};
use crate::storage::kv::{Key, Value};

#[derive(Snafu, Debug)]
pub enum MemTableError {
    #[snafu(context(false), display("WAL operation failed: {source}"))]
    WalOp { source: wal::WalError },
}

pub struct MemTableImpl<W: Wal> {
    id: u64,
    btree: BTree<Key, Value>,
    wal: W,
    max_size: usize,
    current_size: usize,
}

/// The MemTableImpl is an in-memory sorted structure that provides
/// high-performance writes and reads. It is backed by a Write-Ahead Log (WAL)
/// to ensure durability in the event of a crash.
impl<W: Wal> MemTableImpl<W> {
    /// Inserts a key-value pair.
    pub fn put(&mut self, key: Key, value: Value) -> Result<(), MemTableError> {
        self.wal.append(&key, &value)?;
        self.current_size += key.len() as usize + value.len() as usize;
        self.btree.put(key, value);
        Ok(())
    }

    /// Retrieves a value from the BTree.
    pub fn get(&self, key: &Key) -> Option<&Value> {
        self.btree.get(key).map(|v| v)
    }

    pub fn kv_iter(&self) -> impl KVIter<Key = Key, Value = Value> {
        self.btree.kv_iter()
    }

    /// Returns an iterator over the contents of the MemTable.
    pub fn iter(&self) -> impl Iterator<Item = (&Key, &Value)> {
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

pub type MemTableProd = MemTableImpl<wal::FileWal>;
pub type MemTableDev = MemTableImpl<wal::MemWal>;

impl MemTableImpl<wal::FileWal> {
    /// Creates a new MemTable with a given ID and size threshold. The WAL is
    /// written to the directory specified by path.
    pub fn new(id: u64, path: impl AsRef<Path>, max_size: usize) -> Result<Self, MemTableError> {
        let wal_path = path.as_ref().join(format!("memtable_{}.wal", id));
        Ok(Self {
            id,
            // TODO (kev-cao): Should set up configurations for WAL mode and
            // BTree degree.
            wal: wal::FileWal::new(wal_path, wal::SyncMode::Async)?,
            btree: BTree::new(16),
            current_size: 0,
            max_size,
        })
    }

    /// Recovers a MemTable from an existing WAL file.
    pub fn recover(
        id: u64,
        path: impl AsRef<Path>,
        max_size: usize,
    ) -> Result<Self, MemTableError> {
        let wal_path = path.as_ref().join(format!("memtable_{}.wal", id));
        let kvs = wal::FileWal::recover(&wal_path)?;
        // TODO (kev-cao): Configurations for BTree degree.
        let mut btree: BTree<Key, Value> = BTree::new(16);
        let mut size: usize = 0;
        for (k, v) in kvs {
            size += k.len() as usize + v.len() as usize;
            btree.put(k, v);
        }
        Ok(Self {
            id,
            // TODO (kev-cao): Same here.
            wal: wal::FileWal::open(wal_path, wal::SyncMode::Async)?,
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
    use crate::testutils::{generate_rand_bytes, key, seeded_rng, value};
    use rand::Rng;
    use tempfile::tempdir;

    const MAX_TEST_KEY_LEN: usize = u16::MAX as usize;
    const MAX_TEST_VAL_LEN: usize = u16::MAX as usize;

    #[test]
    fn put_get_roundtrip() {
        let (mut rng, _) = seeded_rng(None);
        let mut memtable = MemTableDev::new(1024);

        let mut appended: Vec<(Key, Value)> = Vec::new();
        for _ in 0..100 {
            let key_len = rng.random_range(0..=MAX_TEST_KEY_LEN);
            let val_len = rng.random_range(0..=MAX_TEST_VAL_LEN);
            let key = key(&generate_rand_bytes(key_len, &mut rng));
            let value = value(&generate_rand_bytes(val_len, &mut rng));

            appended.push((key.clone(), value.clone()));
            memtable
                .put(key, value)
                .expect("failed to put into memtable");
        }

        for (k, v) in &appended {
            assert_eq!(
                memtable
                    .get(k)
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
            .put(key(b"key".as_ref()), value(b"value".as_ref()))
            .expect("failed to put into memtable");

        assert!(
            !memtable.is_full(),
            "memtable should not be full after put with size less than max_size",
        );

        memtable
            .put(key(b"key2".as_ref()), value(b"value2".as_ref()))
            .expect("failed to put into memtable");

        assert!(
            memtable.is_full(),
            "memtable should be full after put with size equal to or greater than max_size",
        )
    }

    #[test]
    fn recover() {
        let tmp = tempdir().expect("failed to create temp dir");
        let mut memtable =
            MemTableProd::new(0, tmp.path(), 1024).expect("failed to create file-backed memtable");

        for i in 0..100 {
            let key = key(format!("key{:02}", i).as_ref());
            let value = value(format!("value{:02}", i).as_ref());
            memtable
                .put(key, value)
                .expect("failed to put into memtable");
        }

        memtable.sync().expect("failed to sync memtable");

        let recovered =
            MemTableProd::recover(0, tmp.path(), 1024).expect("failed to recover memtable");

        let mut recovered_iter = recovered.iter();

        for i in 0..100 {
            let expected_key = key(format!("key{:02}", i).as_ref());
            let expected_value = value(format!("value{:02}", i).as_ref());

            let (k, v) = recovered_iter
                .next()
                .expect("recovered memtable should have expected number of entries");

            assert_eq!(
                k, &expected_key,
                "recovered memtable did not return expected key @ index {}",
                i
            );
            assert_eq!(
                v, &expected_value,
                "recovered memtable did not return expected value @ index {}",
                i
            )
        }
    }
}
