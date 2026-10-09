use std::{
    borrow::Borrow,
    io::{self, Seek, SeekFrom},
    mem,
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
    /// An invariant should be upheld that if `curr_block` is `Some`, then the
    /// cursor is always positioned on valid data read from the block.
    curr_block: Option<CurrentBlock<'a>>,
    /// This serves as an intermediary buffer for reading blocks from the
    /// SSTable. This avoids new allocations when reading blocks, while also
    /// preventing read errors from corrupting the curr_block state.
    scratch: Vec<u8>,
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
            scratch: vec![],
            sstable,
        }
    }

    fn seek_ge_in_block<Q>(&mut self, key: &Q) -> Result<bool, SsTableError>
    where
        Key: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let Some(curr_block) = &mut self.curr_block else {
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
        self.value = Some(Value::decode(&mut curr_block.cursor)?);
        self.key = Some(curr_key);
        Ok(true)
    }

    /// Resets the iterator.
    pub fn reset(&mut self) {
        self.key = None;
        self.value = None;
        self.curr_block = None;
    }

    /// Loads the block at the given index into the iterator. Returns true if the
    /// block was loaded, false if the index is out of bounds.
    fn load_block_n(&mut self, n: usize) -> Result<bool, SsTableError> {
        let Some(entry) = self.sstable.index.0.get(n) else {
            return Ok(false);
        };
        self.scratch.resize(entry.block_size as usize, 0);
        self.sstable
            .file
            .read_exact_at(self.scratch.as_mut_slice(), entry.block_offset)?;
        let Some(block) = &mut self.curr_block else {
            self.curr_block = Some(CurrentBlock {
                entry,
                entry_index: n,
                cursor: io::Cursor::new(mem::take(&mut self.scratch)),
            });
            return Ok(true);
        };
        mem::swap(block.cursor.get_mut(), &mut self.scratch);
        block.cursor.set_position(0);
        block.entry_index = n;
        block.entry = entry;
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
        let mut curr_block = match &mut self.curr_block {
            Some(block) => block,
            None => {
                if !self.load_block_n(0)? {
                    self.key = None;
                    self.value = None;
                    return Ok(false);
                }
                self.curr_block.as_mut().unwrap()
            }
        };
        if curr_block.cursor.position() >= curr_block.cursor.get_ref().len() as u64 {
            let next_idx = curr_block.entry_index + 1;
            if !self.load_block_n(next_idx)? {
                self.key = None;
                self.value = None;
                return Ok(false);
            }
            curr_block = self.curr_block.as_mut().unwrap();
        }
        let next_key = Some(Key::decode(&mut curr_block.cursor)?);
        let next_value = Some(Value::decode(&mut curr_block.cursor)?);
        self.key = next_key;
        self.value = next_value;
        Ok(true)
    }

    fn seek_ge<Q>(&mut self, key: &Q) -> Result<bool, Self::Error>
    where
        Self::Key: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let Some((_, entry_index)) = self.sstable.index.key_block(key) else {
            return Ok(false);
        };
        if let Some(curr_block) = &self.curr_block
            && curr_block.entry_index == entry_index
        {
            // We are already in the correct block, just seek within it.
            return self.seek_ge_in_block(key);
        }
        self.load_block_n(entry_index)?;
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
    use super::super::SsTableOpts;
    use super::super::testutils::*;
    use super::*;
    use crate::testutils::{TestResult, key, value};
    use rstest::{fixture, rstest};

    #[fixture]
    fn static_sstable() -> SsTableFixture {
        let kvs = vec![
            (key(b"apple"), value(b"fruit")),
            (key(b"banana"), value(b"fruit")),
            (key(b"cabbage"), value(b"vegetable")),
            (key(b"carrot"), value(b"vegetable")),
            (key(b"durian"), value(b"fruit")),
        ];
        // We artificially lower the block size to force multiple blocks to be
        // created for the test data.
        create_sstable_with_kvs(
            kvs,
            SsTableOpts {
                index_opts: index::SsTableIndexOpts { block_max_size: 48 },
                ..Default::default()
            },
        )
        .expect("failed to create static SSTable fixture")
    }

    #[rstest]
    #[case::seek_to_existing_key(key(b"banana"), Some(key(b"banana")))]
    #[case::seek_to_before_key(key(b"bad apple"), Some(key(b"banana")))]
    #[case::seek_to_first_key(key(b"apple"), Some(key(b"apple")))]
    #[case::seek_to_before_first_key(key(b"acerola"), Some(key(b"apple")))]
    #[case::seek_to_after_last_key(key(b"dwarf mulberry"), None)]
    fn seek_ge(
        static_sstable: SsTableFixture,
        #[case] seek_key: Key,
        #[case] expected_key: Option<Key>,
    ) -> TestResult<()> {
        let SsTableFixture {
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

    #[rstest]
    fn next(static_sstable: SsTableFixture) -> TestResult<()> {
        let SsTableFixture {
            _dir,
            sstable,
            entries,
        } = static_sstable;

        let mut iter = sstable.kv_iter();
        for (expected_key, expected_value) in entries {
            let has_next = iter.next()?;
            assert!(has_next);
            assert_eq!(iter.key()?, Some(&expected_key));
            assert_eq!(iter.value()?, Some(&expected_value));
        }
        assert!(!iter.next()?);
        Ok(())
    }

    #[rstest]
    #[case::seek_to_first_key(key(b"apple"), Some(key(b"banana")))]
    #[case::seek_before_first_key(key(b"acerola"), Some(key(b"banana")))]
    #[case::seek_to_second_to_last_key(key(b"carrot"), Some(key(b"durian")))]
    #[case::seek_to_before_last_key(key(b"date"), None)]
    #[case::seek_to_last_key(key(b"durian"), None)]
    fn seek_ge_next(
        static_sstable: SsTableFixture,
        #[case] seek_key: Key,
        #[case] next_key: Option<Key>,
    ) -> TestResult<()> {
        let mut iter = static_sstable.sstable.kv_iter();
        let found = iter.seek_ge(&seek_key)?;
        assert!(found, "test assumes seek_ge should find a key");
        let has_next = iter.next()?;
        if let Some(next_key) = next_key {
            assert!(has_next);
            assert_eq!(iter.key()?, Some(&next_key));
            assert_eq!(iter.value()?, static_sstable.get_value(&next_key));
        } else {
            assert!(!has_next);
            assert_eq!(iter.key()?, None);
        }
        Ok(())
    }
}
