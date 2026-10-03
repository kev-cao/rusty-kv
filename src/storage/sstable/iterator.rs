use std::{
    borrow::Borrow,
    io::{self, Seek, SeekFrom},
    os::unix::fs::FileExt,
};

use super::{SsTableError, index};
use crate::storage::sstable::SsTable;
use crate::storage::{
    collections::KVIterator,
    encoding::Codable,
    kv::{Key, Value},
};

pub struct SsTableIterator<'a> {
    sstable: &'a SsTable,
    key: Option<Key>,
    value: Option<Value>,
    curr_block: Option<CurrentBlock<'a>>,
}

struct CurrentBlock<'a> {
    entry: &'a index::SsTableIndexEntry,
    entry_index: usize,
    cursor: io::Cursor<Vec<u8>>,
}

impl<'a> SsTableIterator<'a> {
    pub fn new(sstable: &'a SsTable) -> Self {
        Self {
            key: None,
            value: None,
            curr_block: None,
            sstable,
        }
    }

    fn seek_ge_in_block<Q>(&mut self, key: &Q) -> Result<bool, SsTableError>
    where
        Key: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.key = None;
        self.value = None;
        let Some(curr_block) = &mut self.curr_block else {
            println!("No current block found for key");
            return Ok(false);
        };
        curr_block.cursor.set_position(0);
        let mut curr_key = Key::decode(&mut curr_block.cursor)?;
        while curr_key.borrow() < key {
            let value_len = Value::decode_len(&mut curr_block.cursor)?;
            curr_block
                .cursor
                .seek(SeekFrom::Current(value_len.into()))?;
            if curr_block.cursor.position() >= curr_block.entry.block_size.into() {
                return Ok(false);
            }
            curr_key = Key::decode(&mut curr_block.cursor)?;
        }
        self.key = Some(curr_key);
        self.value = Some(Value::decode(&mut curr_block.cursor)?);
        Ok(true)
    }
}

impl<'a> KVIterator for SsTableIterator<'a> {
    type Key = Key;
    type Value = Value;
    type Error = SsTableError;

    fn key(&self) -> Result<Option<&Self::Key>, Self::Error> {
        Ok(self.key.as_ref())
    }

    fn value(&self) -> Result<Option<&Self::Value>, Self::Error> {
        Ok(self.value.as_ref())
    }

    fn next(&mut self) -> Result<bool, Self::Error> {
        unimplemented!()
    }

    fn seek_ge<Q>(&mut self, key: &Q) -> Result<bool, Self::Error>
    where
        Self::Key: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let Some((entry, entry_index)) = self.sstable.index.key_block(key) else {
            return Ok(false);
        };
        if let Some(curr_block) = &self.curr_block
            && curr_block.entry_index == entry_index
        {
            // We are already in the correct block, just seek within it.
            return self.seek_ge_in_block(key);
        }
        let curr_block = match &mut self.curr_block {
            Some(block) => block,
            None => {
                self.curr_block = Some(CurrentBlock {
                    entry,
                    entry_index,
                    cursor: io::Cursor::new(vec![0; entry.block_size as usize]),
                });
                self.curr_block.as_mut().unwrap()
            }
        };
        curr_block
            .cursor
            .get_mut()
            .resize(entry.block_size as usize, 0);
        self.sstable
            .file
            .read_exact_at(curr_block.cursor.get_mut(), entry.block_offset)?;
        self.seek_ge_in_block(key)
    }

    fn seek_lt<Q>(&mut self, key: &Q) -> Result<bool, Self::Error>
    where
        Self::Key: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        unimplemented!()
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutils::*;
    use super::*;
    use crate::testutils::{TestResult, key, value};
    use rstest::{fixture, rstest};

    #[fixture]
    fn static_sstable() -> SSTableFixture {
        let kvs = vec![
            (key(b"apple"), value(b"fruit")),
            (key(b"banana"), value(b"fruit")),
            (key(b"carrot"), value(b"vegetable")),
            (key(b"cabbage"), value(b"vegetable")),
            (key(b"date"), value(b"fruit")),
        ];
        create_sstable_with_kvs(kvs).expect("failed to create static SSTable fixture")
    }

    #[rstest]
    #[case::seek_to_existing_key(key(b"banana"), Some(key(b"banana")))]
    #[case::seek_to_before_key(key(b"bad apple"), Some(key(b"banana")))]
    #[case::seek_to_first_key(key(b"apple"), Some(key(b"apple")))]
    #[case::seek_to_before_first_key(key(b"acerola"), Some(key(b"apple")))]
    #[case::seek_to_after_last_key(key(b"dates"), None)]
    fn seek_ge(
        static_sstable: SSTableFixture,
        #[case] seek_key: Key,
        #[case] expected_key: Option<Key>,
    ) -> TestResult<()> {
        let SSTableFixture {
            _dir,
            sstable,
            entries: _,
        } = static_sstable;

        let mut iter = sstable.kv_iter();
        let found = iter.seek_ge(&seek_key)?;
        if let Some(expected_key) = expected_key {
            assert!(found);
            assert_eq!(iter.key()?, Some(&expected_key));
        } else {
            assert!(!found);
            assert_eq!(iter.key()?, None);
        }
        Ok(())
    }
}
