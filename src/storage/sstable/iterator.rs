use std::{
    borrow::Borrow,
    io::{self},
    os::unix::fs::FileExt,
};

use crate::storage::sstable::{SsTable, SsTableError, block};
use crate::storage::{
    collections::KVIter,
    kv::{Key, Value},
};
use snafu::prelude::*;

#[derive(Snafu, Debug)]
pub enum SstIterError {
    #[snafu(context(false), display("SSTable io operation failed"))]
    Io {
        source: io::Error,
        backtrace: snafu::Backtrace,
    },
    #[snafu(context(false), display("SSTable error: {source}"))]
    SsTable {
        source: SsTableError,
        backtrace: snafu::Backtrace,
    },
    #[snafu(context(false), display("Block error: {source}"))]
    Block {
        source: block::BlockIterError,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("SSTable iterator is poisoned, reset or re-seek required"))]
    IterPoisoned { backtrace: snafu::Backtrace },
    #[snafu(whatever, display("{message}"))]
    Whatever {
        message: String,
        #[snafu(source(from(Box<dyn std::error::Error>, Some)))]
        source: Option<Box<dyn std::error::Error>>,
    },
}

#[derive(Debug, PartialEq, Eq)]
enum SstIterState {
    /// The iterator is positioned at a valid key-value pair.
    Valid,
    /// The iterator has been exhausted and requires a `reset` or `seek` before
    /// it can be used again.
    Exhausted,
    /// The iterator has encountered an error and is in a poisoned state. A
    /// `reset` or `seek` is required before it can be used again.
    Poisoned,
}

pub struct SstIter<'a> {
    sstable: &'a SsTable,
    entry_index: usize,
    /// The iterator for the current block. If None, no block has been loaded.
    block_iter: Option<block::BlockIter>,
    state: SstIterState,
}

impl<'a> SstIter<'a> {
    pub fn new(sstable: &'a SsTable) -> Self {
        Self {
            entry_index: 0,
            block_iter: None,
            state: SstIterState::Valid,
            sstable,
        }
    }

    /// Resets the iterator.
    pub fn reset(&mut self) {
        self.state = SstIterState::Valid;
        self.block_iter = None;
    }

    /// Loads the block at the given index into the iterator. Returns the loaded
    /// iter if successful, or None if the index is out of bounds.
    fn load_block_n(&mut self, n: usize) -> Result<Option<&mut block::BlockIter>, SstIterError> {
        let Some(entry) = self.sstable.index.0.get(n) else {
            self.state = SstIterState::Exhausted;
            return Ok(None);
        };

        let mut block_data = vec![0; entry.block_size as usize];
        self.sstable
            .file
            .read_exact_at(block_data.as_mut_slice(), entry.block_offset)
            .inspect_err(|_| {
                self.block_iter = None;
            })?;
        self.entry_index = n;
        self.block_iter = Some(block::BlockIter::new(block::Block::decode(
            &mut block_data.as_slice(),
            entry.block_size as usize,
        )?));
        Ok(self.block_iter.as_mut())
    }

    fn next_inner(&mut self) -> Result<bool, SstIterError> {
        match self.state {
            SstIterState::Exhausted => {
                return Ok(false);
            }
            SstIterState::Poisoned => {
                IterPoisonedSnafu.fail()?;
            }
            _ => {}
        };
        let block_iter = match &mut self.block_iter {
            Some(block_iter) => block_iter,
            None => {
                let Some(block_iter) = self.load_block_n(0)? else {
                    self.set_exhausted();
                    return Ok(false);
                };
                block_iter
            }
        };
        if block_iter.next()? {
            return Ok(true);
        }
        let next_idx = self.entry_index + 1;
        let Some(block_iter) = self.load_block_n(next_idx)? else {
            self.set_exhausted();
            return Ok(false);
        };
        Ok(block_iter.next()?)
    }

    fn seek_ge_inner<Q>(&mut self, key: &Q) -> Result<bool, SstIterError>
    where
        Key: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.state = SstIterState::Valid;
        let Some((_, entry_index)) = self.sstable.index.key_block(key) else {
            self.set_exhausted();
            return Ok(false);
        };
        if let Some(block_iter) = &mut self.block_iter
            && self.entry_index == entry_index
        {
            return Ok(block_iter.seek_ge(key)?);
        }
        let Some(block_iter) = self.load_block_n(entry_index)? else {
            return Ok(false);
        };
        Ok(block_iter.seek_ge(key)?)
    }

    fn seek_lt_inner<Q>(&mut self, key: &Q) -> Result<bool, SstIterError>
    where
        Key: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.state = SstIterState::Valid;
        // Let there be two adjacent blocks with last keys α and β, where α < β,
        // called b-α and b-β. Assume b-α and b-β are potential candidates for
        // containing the search query ε.
        //
        // There are three cases to consider:
        // 1. ε < α < β
        //  - In this case, we should seek in b-α.
        // 2. α < ε < β
        //  - In this case, we should seek in b-β, but if we are unable to
        //    find a satisfactory key < ε, we return the last key in b-α.
        // 3. α < β < ε
        //  - We should return the last key in b-β.
        //
        // Since our index lookup returns the block the key should be in, then
        // the returned index is always that of b-β.
        let Some((_, beta_entry_idx)) = self.sstable.index.key_block(key) else {
            // The key lasts beyond the last key in the index, so case 3.
            let last_block_idx = self.sstable.index.0.len() - 1;
            let Some(block_iter) = self.load_block_n(last_block_idx)? else {
                whatever!("should not happen, as we should have picked a valid block to load");
            };
            block_iter.seek_to_last()?;
            return Ok(true);
        };
        let Some(block_iter) = self.load_block_n(beta_entry_idx)? else {
            whatever!("should not happen, as we should have picked a valid block to load");
        };
        if block_iter.seek_lt(key)? {
            return Ok(true);
        }
        if beta_entry_idx == 0 {
            // We are already at the first block, so there is no previous block to
            // seek in.
            self.set_exhausted();
            return Ok(false);
        }
        let Some(block_iter) = self.load_block_n(beta_entry_idx - 1)? else {
            return Ok(false);
        };
        block_iter.seek_to_last()?;
        Ok(true)
    }

    fn set_exhausted(&mut self) {
        self.block_iter = None;
        self.state = SstIterState::Exhausted;
    }
}

impl<'a> KVIter for SstIter<'a> {
    type Key = Key;
    type Value = Value;
    type Error = SstIterError;

    fn key(&self) -> Result<Option<&Self::Key>, Self::Error> {
        match &self.block_iter {
            Some(block_iter) => Ok(block_iter.key()?),
            None => Ok(None),
        }
    }

    fn value(&self) -> Result<Option<&Self::Value>, Self::Error> {
        match &self.block_iter {
            Some(block_iter) => Ok(block_iter.value()?),
            None => Ok(None),
        }
    }

    fn next(&mut self) -> Result<bool, Self::Error> {
        self.next_inner().inspect_err(|_| {
            self.state = SstIterState::Poisoned;
        })
    }

    fn seek_ge<Q>(&mut self, key: &Q) -> Result<bool, Self::Error>
    where
        Self::Key: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.seek_ge_inner(key).inspect_err(|_| {
            self.state = SstIterState::Poisoned;
        })
    }

    fn seek_lt<Q>(&mut self, key: &Q) -> Result<bool, Self::Error>
    where
        Self::Key: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.seek_lt_inner(key).inspect_err(|_| {
            self.state = SstIterState::Poisoned;
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::SstBuildOpts;
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
            tiny_block_opts(),
            large_block_opts(),
            massive_block_opts(),
            small_restart_opts(),
            large_restart_opts(),
            massive_block_and_restart_opts()
        )]
        opts: SstBuildOpts,
        #[case] seek_key: Key,
        #[case] expected_key: Option<Key>,
    ) -> TestResult<()> {
        let SstFixture {
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
            tiny_block_opts(),
            large_block_opts(),
            massive_block_opts(),
            small_restart_opts(),
            large_restart_opts(),
            massive_block_and_restart_opts()
        )]
        opts: SstBuildOpts,
    ) -> TestResult<()> {
        let SstFixture {
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
            tiny_block_opts(),
            large_block_opts(),
            massive_block_opts(),
            small_restart_opts(),
            large_restart_opts(),
            massive_block_and_restart_opts()
        )]
        opts: SstBuildOpts,
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
    #[case::seek_to_existing_key(key(b"floccipend"), Some(key(b"floccinaucinihilipilification")))]
    #[case::seek_to_between_keys(key(b"axe"), Some(key(b"approximate")))]
    #[case::seek_to_first_key(key(b"apple"), None)]
    #[case::seek_to_before_first_key(key(b"acerola"), None)]
    #[case::seek_to_last_key(key(b"xray"), Some(key(b"transcription")))]
    #[case::seek_to_after_last_key(key(b"zebra"), Some(key(b"xray")))]
    fn seek_lt(
        default_kvs: Vec<(Key, Value)>,
        #[values(
            normal_block_opts(),
            small_block_opts(),
            tiny_block_opts(),
            large_block_opts(),
            massive_block_opts(),
            small_restart_opts(),
            large_restart_opts(),
            massive_block_and_restart_opts()
        )]
        opts: SstBuildOpts,
        #[case] seek_key: Key,
        #[case] expected_key: Option<Key>,
    ) -> TestResult<()> {
        let SstFixture {
            sstable,
            dir: _dir,
            filepath: _,
            entries: _,
        } = create_sstable_with_kvs(default_kvs, opts)?;
        let mut iter = sstable.kv_iter();
        let found = iter.seek_lt(&seek_key)?;
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
    #[case::seek_to_existing_key(key(b"floccipend"), Some(key(b"floccipend")))]
    #[case::seek_to_second_key(key(b"application"), Some(key(b"application")))]
    #[case::seek_to_between_keys(key(b"axe"), Some(key(b"banana")))]
    fn seek_lt_next(
        default_kvs: Vec<(Key, Value)>,
        #[values(
            normal_block_opts(),
            tiny_block_opts(),
            small_block_opts(),
            large_block_opts(),
            massive_block_opts(),
            small_restart_opts(),
            large_restart_opts(),
            massive_block_and_restart_opts()
        )]
        opts: SstBuildOpts,
        #[case] seek_key: Key,
        #[case] expected_key: Option<Key>,
    ) -> TestResult<()> {
        let fixture = create_sstable_with_kvs(default_kvs, opts)?;
        let mut iter = fixture.sstable.kv_iter();
        let found = iter.seek_lt(&seek_key)?;
        let has_next = iter.next()?;
        if let Some(expected_key) = expected_key {
            assert!(found);
            assert!(has_next);
            assert_eq!(iter.key()?, Some(&expected_key));
            assert_eq!(iter.value()?, fixture.get_value(&expected_key));
        } else {
            assert!(!found);
            assert!(!has_next);
            assert_eq!(iter.key()?, None);
        }
        Ok(())
    }

    #[rstest]
    fn reset(
        default_kvs: Vec<(Key, Value)>,
        #[values(normal_block_opts())] opts: SstBuildOpts,
    ) -> TestResult<()> {
        let SstFixture {
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
        assert_eq!(iter.state, SstIterState::Exhausted);
        iter.reset();
        assert!(iter.next()?);
        assert_eq!(iter.key()?, Some(&entries[0].0));
        assert_eq!(iter.value()?, Some(&entries[0].1));
        Ok(())
    }

    #[rstest]
    fn poisoned_iter(default_kvs: Vec<(Key, Value)>) -> TestResult<()> {
        let SstFixture {
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
        let verify_success_iter_and_fail = |iter: &mut SstIter| {
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
                SstIterError::IterPoisoned { .. },
            );
        };

        verify_success_iter_and_fail(&mut iter);
        iter.reset();
        verify_success_iter_and_fail(&mut iter);

        // Using either seek instead of reset after poisoning should also work.
        iter.seek_ge(&key(b"apple"))?;
        assert_eq!(iter.key()?, Some(&key(b"apple")));

        verify_success_iter_and_fail(&mut iter);
        iter.seek_lt(&key(b"application"))?;
        assert_eq!(iter.key()?, Some(&key(b"apple")));
        Ok(())
    }

    // Defines a set of block configs for the tests that want coverage over
    // different SSTable block shapes. For the purposes of the tests, we'll
    // assume that a block size of 128 bytes and a restart interval of 6 is
    // considered "normal" since the tests are using a small number of keys and
    // values.
    fn normal_block_opts() -> SstBuildOpts {
        SstBuildOpts {
            block_opts: block::BlockBuilderOpts {
                max_size: 128,
                restart_interval: 6,
            },
            ..Default::default()
        }
    }

    fn small_block_opts() -> SstBuildOpts {
        SstBuildOpts {
            block_opts: block::BlockBuilderOpts {
                max_size: 32,
                restart_interval: 6,
            },
            ..Default::default()
        }
    }

    /// Tiny block size to force each key/value into its own block.
    fn tiny_block_opts() -> SstBuildOpts {
        SstBuildOpts {
            block_opts: block::BlockBuilderOpts {
                max_size: 8,
                restart_interval: 6,
            },
            ..Default::default()
        }
    }

    fn large_block_opts() -> SstBuildOpts {
        SstBuildOpts {
            block_opts: block::BlockBuilderOpts {
                max_size: 256,
                restart_interval: 6,
            },
            ..Default::default()
        }
    }

    fn massive_block_opts() -> SstBuildOpts {
        SstBuildOpts {
            block_opts: block::BlockBuilderOpts {
                max_size: 1024,
                restart_interval: 6,
            },
            ..Default::default()
        }
    }

    fn small_restart_opts() -> SstBuildOpts {
        SstBuildOpts {
            block_opts: block::BlockBuilderOpts {
                max_size: 128,
                restart_interval: 2,
            },
            ..Default::default()
        }
    }

    fn large_restart_opts() -> SstBuildOpts {
        SstBuildOpts {
            block_opts: block::BlockBuilderOpts {
                max_size: 128,
                restart_interval: 12,
            },
            ..Default::default()
        }
    }

    /// Large enough to store all of the test keys and values in a single block,
    /// with a single restart point.
    fn massive_block_and_restart_opts() -> SstBuildOpts {
        SstBuildOpts {
            block_opts: block::BlockBuilderOpts {
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
