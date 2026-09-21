use crate::storage::{
    memtable::{MemTableImpl, wal::Wal},
    sstable::SsTable,
};

pub trait StorageEngine {
    fn put(&self, key: Vec<u8>, value: Vec<u8>);

    fn get(&self, key: &[u8]) -> Option<&[u8]>;
}

pub struct StorageEngineImpl<W: Wal> {
    memtable: MemTableImpl<W>,
    frozen_memtables: Vec<MemTableImpl<W>>,
    sstables: Vec<Vec<SsTable>>,
}
