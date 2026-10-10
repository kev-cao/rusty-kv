use std::{
    borrow::Borrow,
    io::{self, Seek, SeekFrom},
    os::unix::fs::FileExt,
};

use super::index;
use crate::storage::sstable::SsTable;
use crate::storage::{
    collections::KVIterator,
    encoding::Codable,
    kv::{Key, Value},
};
use snafu::prelude::*;

#[derive(Snafu, Debug)]
pub enum SsTableIteratorError {
    #[snafu(context(false), display("SSTable io operation failed"))]
    IoError {
        source: io::Error,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("SSTable iterator is poisoned, reset or re-seek required"))]
    IteratorPoisoned { backtrace: snafu::Backtrace },
    #[snafu(whatever, display("{message}"))]
    Whatever {
        message: String,
        #[snafu(source(from(Box<dyn std::error::Error>, Some)))]
        source: Option<Box<dyn std::error::Error>>,
    },
}

#[derive(Debug, PartialEq, Eq)]
enum SsTableIteratorState {
    /// The iterator is positioned at a valid key-value pair.
    Valid,
    /// The iterator has been exhausted and requires a `reset` or `seek` before
    /// it can be used again.
    Exhausted,
    /// The iterator has encountered an error and is in a poisoned state. A
    /// `reset` or `seek` is required before it can be used again.
    Poisoned,
}

pub struct SsTableIterator<'a> {
    sstable: &'a SsTable,
    key: Option<Key>,
    value: Option<Value>,
    curr_block: Option<CurrentBlock<'a>>,
    state: SsTableIteratorState,
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
            state: SsTableIteratorState::Valid,
            sstable,
        }
    }

    /// Resets the iterator.
    pub fn reset(&mut self) {
        self.key = None;
        self.value = None;
        self.state = SsTableIteratorState::Valid;
        self.curr_block = None;
    }

    /// Loads the block at the given index into the iterator. Returns true if the
    /// block was loaded, false if the index is out of bounds.
    fn load_block_n(&mut self, n: usize) -> Result<bool, SsTableIteratorError> {
        let Some(entry) = self.sstable.index.0.get(n) else {
            self.state = SsTableIteratorState::Exhausted;
            return Ok(false);
        };
        let block = match &mut self.curr_block {
            Some(block) => block,
            None => {
                self.curr_block = Some(CurrentBlock {
                    entry,
                    entry_index: n,
                    cursor: io::Cursor::new(vec![0; entry.block_size as usize]),
                });
                self.curr_block.as_mut().unwrap()
            }
        };
        block.entry_index = n;
        block.entry = entry;
        block.cursor.set_position(0);
        block.cursor.get_mut().resize(entry.block_size as usize, 0);
        self.sstable
            .file
            .read_exact_at(block.cursor.get_mut(), entry.block_offset)
            .inspect_err(|_| {
                self.curr_block = None;
            })?;
        Ok(true)
    }

    fn next_inner(&mut self) -> Result<bool, SsTableIteratorError> {
        match self.state {
            SsTableIteratorState::Exhausted => {
                return Ok(false);
            }
            SsTableIteratorState::Poisoned => {
                IteratorPoisonedSnafu.fail()?;
            }
            _ => {}
        };

        let mut curr_block = match &mut self.curr_block {
            Some(block) => block,
            None => {
                if !self.load_block_n(0)? {
                    self.set_exhausted();
                    return Ok(false);
                }
                self.curr_block.as_mut().unwrap()
            }
        };
        if curr_block.cursor.position() >= curr_block.cursor.get_ref().len() as u64 {
            let next_idx = curr_block.entry_index + 1;
            if !self.load_block_n(next_idx)? {
                self.set_exhausted();
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

    fn seek_ge_inner<Q>(&mut self, key: &Q) -> Result<bool, SsTableIteratorError>
    where
        Key: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.state = SsTableIteratorState::Valid;
        let Some((_, entry_index)) = self.sstable.index.key_block(key) else {
            self.set_exhausted();
            return Ok(false);
        };
        if let Some(curr_block) = &self.curr_block
            && curr_block.entry_index == entry_index
        {
            // We are already in the correct block, just seek within it.
            self.seek_ge_in_block(key)?;
            return Ok(true);
        }
        if !self.load_block_n(entry_index)? {
            return Ok(false);
        }
        self.seek_ge_in_block(key)?;
        Ok(true)
    }

    fn seek_ge_in_block<Q>(&mut self, key: &Q) -> Result<(), SsTableIteratorError>
    where
        Key: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let Some(curr_block) = &mut self.curr_block else {
            whatever!("unexpectedly called seek_ge_in_block with no current block loaded");
        };
        curr_block.cursor.set_position(0);
        let mut curr_key = Key::decode(&mut curr_block.cursor)?;
        while curr_key.borrow() < key {
            let value_len = Value::decode_len(&mut curr_block.cursor)?;
            curr_block
                .cursor
                .seek(SeekFrom::Current(value_len.into()))?;
            if curr_block.cursor.position() >= curr_block.entry.block_size.into() {
                // This technically shouldn't happen, as the index should have
                // pointed us to a block whose last key is >= the seek key.
                whatever!("seek_ge_in_block reached end of block without finding key >= seek key");
            }
            curr_key = Key::decode(&mut curr_block.cursor)?;
        }
        self.value = Some(Value::decode(&mut curr_block.cursor)?);
        self.key = Some(curr_key);
        Ok(())
    }

    fn set_exhausted(&mut self) {
        self.key = None;
        self.value = None;
        self.state = SsTableIteratorState::Exhausted;
    }
}

impl<'a> KVIterator for SsTableIterator<'a> {
    type Key = Key;
    type Value = Value;
    type Error = SsTableIteratorError;

    fn key(&self) -> Result<Option<&Self::Key>, Self::Error> {
        Ok(self.key.as_ref())
    }

    fn value(&self) -> Result<Option<&Self::Value>, Self::Error> {
        Ok(self.value.as_ref())
    }

    fn next(&mut self) -> Result<bool, Self::Error> {
        self.next_inner().inspect_err(|_| {
            self.state = SsTableIteratorState::Poisoned;
        })
    }

    fn seek_ge<Q>(&mut self, key: &Q) -> Result<bool, Self::Error>
    where
        Self::Key: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.seek_ge_inner(key).inspect_err(|_| {
            self.state = SsTableIteratorState::Poisoned;
        })
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
    use std::assert_matches;

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
        #[from(static_sstable)] SsTableFixture {
            sstable,
            dir: _,
            filepath: _,
            entries: _,
        }: SsTableFixture,
        #[case] seek_key: Key,
        #[case] expected_key: Option<Key>,
    ) -> TestResult<()> {
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
    fn next(
        #[from(static_sstable)] SsTableFixture {
            sstable,
            entries,
            dir: _,
            filepath: _,
        }: SsTableFixture,
    ) -> TestResult<()> {
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

    #[rstest]
    fn reset(
        #[from(static_sstable)] SsTableFixture {
            sstable,
            entries,
            dir: _,
            filepath: _,
        }: SsTableFixture,
    ) -> TestResult<()> {
        let mut iter = sstable.kv_iter();
        for _ in 0..entries.len() {
            assert!(iter.next()?);
        }
        assert!(!iter.next()?);
        assert_eq!(iter.key()?, None);
        assert_eq!(iter.value()?, None);
        assert_eq!(iter.state, SsTableIteratorState::Exhausted);
        iter.reset();
        assert!(iter.next()?);
        assert_eq!(iter.key()?, Some(&entries[0].0));
        assert_eq!(iter.value()?, Some(&entries[0].1));
        Ok(())
    }

    #[rstest]
    fn poisoned_iter(
        #[from(static_sstable)] SsTableFixture {
            sstable,
            filepath,
            entries,
            dir: _,
        }: SsTableFixture,
    ) -> TestResult<()> {
        let mut iter = sstable.kv_iter();
        for _ in 0..2 {
            assert!(iter.next()?);
        }
        // Corrupt the SSTable file to simulate an I/O error. In order to
        // validate that the iterator works after poisoning and resetting, we'll
        // only corrupt the file at the point where the iterator is currently
        // positioned.
        let file = std::fs::OpenOptions::new()
            .write(true)
            .read(true)
            .open(&filepath)
            .expect("failed to open SSTable file");
        let curr_block = iter
            .curr_block
            .as_ref()
            .expect("iterator should have a current block");
        let current_pos = curr_block.entry.block_offset + curr_block.cursor.position();
        file.write_at(&[0xFF; 64], current_pos)
            .expect("failed to corrupt SSTable file");

        assert!(iter.next().is_err());
        assert_matches!(
            iter.next()
                .expect_err("expected poisoned iterator to return an error"),
            SsTableIteratorError::IteratorPoisoned { .. },
        );

        iter.reset();
        for (key, value) in entries.iter().take(2) {
            assert!(iter.next()?);
            assert_eq!(iter.key()?, Some(key));
            assert_eq!(iter.value()?, Some(value));
        }

        assert!(iter.next().is_err());
        assert_matches!(
            iter.next()
                .expect_err("expected poisoned iterator to return an error"),
            SsTableIteratorError::IteratorPoisoned { .. }
        );

        // Using a seek instead of reset after poisoning should also work.
        iter.seek_ge(&key(b"apple"))?;
        assert_eq!(iter.key()?, Some(&key(b"apple")));
        Ok(())
    }
}
