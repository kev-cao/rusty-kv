use std::{
    borrow::Borrow,
    io::{self},
    os::unix::fs::FileExt,
};

use super::index;
use crate::storage::sstable::{SsTable, SsTableError, block};
use crate::storage::{
    collections::KVIterator,
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
    #[snafu(context(false), display("SSTable error: {source}"))]
    SsTableError {
        source: SsTableError,
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
    block_iter: Option<BlockIter<'a>>,
    state: SsTableIteratorState,
}

struct BlockIter<'a> {
    entry: &'a index::SsTableIndexEntry,
    entry_index: usize,
    block: block::SsTableBlock,
    /// Restart index represents the index of the restart point that the cursor
    /// is currently at. When a cursor is at the beginning of a block and has
    /// yet to visit the first key, the restart index is -1.
    restart_index: i64,
    key_prefix: Option<Key>,
    offset: usize,
}

impl<'a> SsTableIterator<'a> {
    pub fn new(sstable: &'a SsTable) -> Self {
        Self {
            key: None,
            value: None,
            block_iter: None,
            state: SsTableIteratorState::Valid,
            sstable,
        }
    }

    /// Resets the iterator.
    pub fn reset(&mut self) {
        self.key = None;
        self.value = None;
        self.state = SsTableIteratorState::Valid;
        self.block_iter = None;
    }

    /// Loads the block at the given index into the iterator. Returns true if the
    /// block was loaded, false if the index is out of bounds.
    fn load_block_n(&mut self, n: usize) -> Result<bool, SsTableIteratorError> {
        let Some(entry) = self.sstable.index.0.get(n) else {
            self.state = SsTableIteratorState::Exhausted;
            return Ok(false);
        };

        let mut block_data = vec![0; entry.block_size as usize];
        self.sstable
            .file
            .read_exact_at(block_data.as_mut_slice(), entry.block_offset)
            .inspect_err(|_| {
                self.block_iter = None;
            })?;
        self.block_iter = Some(BlockIter {
            entry,
            entry_index: n,
            key_prefix: None,
            block: block::SsTableBlock::decode(
                &mut block_data.as_slice(),
                entry.block_size as usize,
            )?,
            restart_index: -1,
            offset: 0,
        });
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
        let mut block_iter = match &mut self.block_iter {
            Some(block) => block,
            None => {
                if !self.load_block_n(0)? {
                    self.set_exhausted();
                    return Ok(false);
                }
                self.block_iter.as_mut().unwrap()
            }
        };
        if block_iter.offset >= block_iter.block.data.len() {
            let next_idx = block_iter.entry_index + 1;
            if !self.load_block_n(next_idx)? {
                self.set_exhausted();
                return Ok(false);
            }
            block_iter = self.block_iter.as_mut().unwrap();
        }
        let curr_restart_idx = block_iter.restart_index;
        if curr_restart_idx < block_iter.block.restart_array.len() as i64 - 1
            && block_iter.offset
                == block_iter.block.restart_array[(curr_restart_idx + 1) as usize] as usize
        {
            // The offset has reached the next restart point, so update the
            // index and decode the key-value pair with no prefix.
            let (key, value, bytes_read) =
                block::decode_kv_pair(&mut &block_iter.block.data[block_iter.offset..], None)?;
            block_iter.offset += bytes_read;
            block_iter.key_prefix = Some(key.clone());
            block_iter.restart_index += 1;
            self.key = Some(key);
            self.value = Some(value);
        } else {
            let (key, value, bytes_read) = block::decode_kv_pair(
                &mut &block_iter.block.data[block_iter.offset..],
                block_iter.key_prefix.as_ref(),
            )?;
            block_iter.offset += bytes_read;
            self.key = Some(key);
            self.value = Some(value);
        }
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
        if let Some(curr_block) = &self.block_iter
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
        let Some(block_iter) = &mut self.block_iter else {
            whatever!("unexpectedly called seek_ge_in_block with no current block loaded");
        };
        let mut search_err = None;
        let search_result = block_iter.block.restart_array.binary_search_by(|offset| {
            match block::decode_key(&mut &block_iter.block.data[*offset as usize..], None) {
                Ok((restart_key, _, _)) => restart_key.borrow().cmp(key),
                Err(e) => {
                    search_err = Some(e);
                    std::cmp::Ordering::Equal
                }
            }
        });
        if let Some(e) = search_err {
            return Err(e.into());
        }
        // We set the restart index to the index *prior* of the restart interval
        // we wish to seek within, so that we can just re-use the `next_inner()`
        // logic. Setting to the prior index positions us at the start of the
        // restart interval.
        match search_result {
            Ok(restart_idx) => {
                block_iter.restart_index = restart_idx as i64 - 1;
                block_iter.offset = block_iter.block.restart_array[restart_idx] as usize;
            }
            Err(restart_idx) => {
                if restart_idx == 0 {
                    block_iter.restart_index = -1;
                    block_iter.offset = 0;
                } else {
                    block_iter.restart_index = restart_idx as i64 - 2;
                    block_iter.offset = block_iter.block.restart_array[restart_idx - 1] as usize;
                }
            }
        };
        while self.next_inner()? {
            let Some(curr_key) = &self.key else {
                whatever!("next_inner returned true but key is None");
            };
            if curr_key.borrow() >= key {
                return Ok(());
            }
        }
        // This technically shouldn't happen, as the index should have
        // pointed us to a block whose last key is >= the seek key.
        whatever!("seek_ge_in_block reached end of block without finding key >= seek key");
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
    use super::super::SsTableBuildOpts;
    use super::super::testutils::*;
    use super::*;
    use crate::storage::sstable::block;
    use crate::testutils::{TestResult, key, value};
    use rstest::{fixture, rstest};
    use std::assert_matches;

    #[rstest]
    #[case::seek_to_existing_key(key(b"banana"), Some(key(b"banana")))]
    #[case::seek_to_before_key(key(b"bad apple"), Some(key(b"banana")))]
    #[case::seek_to_first_key(key(b"apple"), Some(key(b"apple")))]
    #[case::seek_to_before_first_key(key(b"acerola"), Some(key(b"apple")))]
    #[case::seek_to_last_key(key(b"xray"), Some(key(b"xray")))]
    #[case::seek_to_after_last_key(key(b"zebra"), None)]
    fn seek_ge(
        default_kvs: Vec<(Key, Value)>,
        #[values(
            normal_block_opts(),
            small_block_opts(),
            large_block_opts(),
            massive_block_opts(),
            small_restart_opts(),
            large_restart_opts(),
            massive_block_and_restart_opts()
        )]
        opts: SsTableBuildOpts,
        #[case] seek_key: Key,
        #[case] expected_key: Option<Key>,
    ) -> TestResult<()> {
        let SsTableFixture {
            sstable,
            dir: _dir,
            filepath: _,
            entries: _,
        } = create_sstable_with_kvs(default_kvs, opts)?;
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
        default_kvs: Vec<(Key, Value)>,
        #[values(
            normal_block_opts(),
            small_block_opts(),
            large_block_opts(),
            massive_block_opts(),
            small_restart_opts(),
            large_restart_opts(),
            massive_block_and_restart_opts()
        )]
        opts: SsTableBuildOpts,
    ) -> TestResult<()> {
        let SsTableFixture {
            sstable,
            entries,
            dir: _dir,
            filepath: _,
        } = create_sstable_with_kvs(default_kvs, opts)?;
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
    #[case::seek_to_first_key(key(b"apple"), Some(key(b"application")))]
    #[case::seek_before_first_key(key(b"acerola"), Some(key(b"application")))]
    #[case::seek_to_second_to_last_key(key(b"transcription"), Some(key(b"xray")))]
    #[case::seek_to_before_last_key(key(b"xenon"), None)]
    #[case::seek_to_last_key(key(b"xray"), None)]
    fn seek_ge_next(
        default_kvs: Vec<(Key, Value)>,
        #[values(
            normal_block_opts(),
            small_block_opts(),
            large_block_opts(),
            massive_block_opts(),
            small_restart_opts(),
            large_restart_opts(),
            massive_block_and_restart_opts()
        )]
        opts: SsTableBuildOpts,
        #[case] seek_key: Key,
        #[case] next_key: Option<Key>,
    ) -> TestResult<()> {
        let static_sstable = create_sstable_with_kvs(default_kvs, opts)?;
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
        default_kvs: Vec<(Key, Value)>,
        #[values(normal_block_opts())] opts: SsTableBuildOpts,
    ) -> TestResult<()> {
        let SsTableFixture {
            sstable,
            entries,
            dir: _dir,
            filepath: _,
        } = create_sstable_with_kvs(default_kvs, opts)?;
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
    fn poisoned_iter(default_kvs: Vec<(Key, Value)>) -> TestResult<()> {
        let SsTableFixture {
            sstable,
            dir: _dir,
            filepath,
            entries: _,
        } = create_sstable_with_kvs(default_kvs, normal_block_opts())?;
        let mut iter = sstable.kv_iter();
        // We corrupt the *second* block in the table, to ensure that we can
        // still iterate the first block successfully and only fail when we
        // attempt to read the second block. This allows us to validate that
        // resetting allows you to read the non-corrupted block.
        let file = std::fs::OpenOptions::new()
            .write(true)
            .read(true)
            .open(&filepath)
            .expect("failed to open SSTable file");
        let next_block = sstable
            .index
            .0
            .get(1)
            .expect("at least two blocks should exist");
        file.write_at(&[0xFF; 64], next_block.block_offset)
            .expect("failed to corrupt SSTable file");

        // Since we corrupted the second block, we expect to be able to iterate
        // the first block successfully and then encounter an error on the
        // second when using a clean iterator.
        let verify_success_iter_and_fail = |iter: &mut SsTableIterator| {
            let mut success_iters = 0;
            let terminating_result = loop {
                let res = iter.next();
                if !res.as_ref().is_ok_and(|has_next| *has_next) {
                    break res;
                } else {
                    success_iters += 1;
                }
            };
            assert!(
                success_iters > 0,
                "expected to successfully iterate at least one key-value pair before encountering an error"
            );
            assert!(
                terminating_result.is_err(),
                "expect iteration to stop due to an error when reading the corrupted block"
            );
            assert_matches!(
                iter.next()
                    .expect_err("expected poisoned iterator to return an error"),
                SsTableIteratorError::IteratorPoisoned { .. },
            );
        };

        verify_success_iter_and_fail(&mut iter);
        iter.reset();
        verify_success_iter_and_fail(&mut iter);

        // Using a seek instead of reset after poisoning should also work.
        iter.seek_ge(&key(b"apple"))?;
        assert_eq!(iter.key()?, Some(&key(b"apple")));
        Ok(())
    }

    // Defines a set of block configs for the tests that want coverage over
    // different SSTable block shapes. For the purposes of the tests, we'll
    // assume that a block size of 128 bytes and a restart interval of 6 is
    // considered "normal" since the tests are using a small number of keys and
    // values.
    fn normal_block_opts() -> SsTableBuildOpts {
        SsTableBuildOpts {
            block_opts: block::SsTableBlockBuilderOpts {
                max_size: 128,
                restart_interval: 6,
            },
            ..Default::default()
        }
    }

    fn small_block_opts() -> SsTableBuildOpts {
        SsTableBuildOpts {
            block_opts: block::SsTableBlockBuilderOpts {
                max_size: 32,
                restart_interval: 6,
            },
            ..Default::default()
        }
    }

    fn large_block_opts() -> SsTableBuildOpts {
        SsTableBuildOpts {
            block_opts: block::SsTableBlockBuilderOpts {
                max_size: 256,
                restart_interval: 6,
            },
            ..Default::default()
        }
    }

    fn massive_block_opts() -> SsTableBuildOpts {
        SsTableBuildOpts {
            block_opts: block::SsTableBlockBuilderOpts {
                max_size: 1024,
                restart_interval: 6,
            },
            ..Default::default()
        }
    }

    fn small_restart_opts() -> SsTableBuildOpts {
        SsTableBuildOpts {
            block_opts: block::SsTableBlockBuilderOpts {
                max_size: 128,
                restart_interval: 2,
            },
            ..Default::default()
        }
    }

    fn large_restart_opts() -> SsTableBuildOpts {
        SsTableBuildOpts {
            block_opts: block::SsTableBlockBuilderOpts {
                max_size: 128,
                restart_interval: 12,
            },
            ..Default::default()
        }
    }

    /// Large enough to store all of the test keys and values in a single block,
    /// with a single restart point.
    fn massive_block_and_restart_opts() -> SsTableBuildOpts {
        SsTableBuildOpts {
            block_opts: block::SsTableBlockBuilderOpts {
                max_size: 1024,
                restart_interval: 32,
            },
            ..Default::default()
        }
    }

    #[fixture]
    fn default_kvs() -> Vec<(Key, Value)> {
        vec![
            (key(b"apple"), value(b"noun")),
            (key(b"application"), value(b"noun")),
            (key(b"apply"), value(b"verb")),
            (key(b"appraise"), value(b"verb")),
            (key(b"apprentice"), value(b"noun")),
            (key(b"approximate"), value(b"adjective")),
            (key(b"banana"), value(b"noun")),
            (key(b"bandana"), value(b"noun")),
            (key(b"cabbage"), value(b"noun")),
            (key(b"carry"), value(b"verb")),
            (key(b"cloudy"), value(b"adjective")),
            (key(b"exceptional"), value(b"adjective")),
            (key(b"floccinaucinihilipilification"), value(b"noun")),
            (key(b"floccipend"), value(b"verb")),
            (key(b"graph"), value(b"noun")),
            (key(b"graphical"), value(b"adjective")),
            (key(b"graphite"), value(b"noun")),
            (key(b"grappling"), value(b"noun")),
            (key(b"grateful"), value(b"adjective")),
            (key(b"gravity"), value(b"noun")),
            (key(b"harbor"), value(b"noun")),
            (key(b"harmonious"), value(b"adjective")),
            (key(b"harmonize"), value(b"verb")),
            (key(b"harpoon"), value(b"noun")),
            (key(b"helicopter"), value(b"noun")),
            (key(b"knowledge"), value(b"noun")),
            (key(b"transact"), value(b"verb")),
            (key(b"transaction"), value(b"noun")),
            (key(b"transactional"), value(b"adjective")),
            (key(b"transcribe"), value(b"verb")),
            (key(b"transcription"), value(b"noun")),
            (key(b"xray"), value(b"noun")),
        ]
    }
}
