use std::os::unix::fs::FileExt;

use crate::storage::sstable::SsTable;

pub struct SsTableIteratorImpl<'a> {
    sstable: &'a SsTable,
}

impl SsTableIteratorImpl<'_> {}
