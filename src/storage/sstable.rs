mod block;
mod filter;
mod footer;
mod index;
mod iterator;
#[cfg(test)]
mod testutils;

use snafu::prelude::*;
use std::{
    fs,
    io::{self, Seek, SeekFrom, Write},
    path::{self, PathBuf},
};

use crate::storage::{
    encoding::Codable,
    sstable::footer::{FOOTER_TAIL_SIZE, Footer},
};
use crate::storage::{
    kv::{Key, Value},
    sstable::footer::FooterTail,
};

pub const SST_VERSION: u16 = 1;

#[derive(Snafu, Debug)]
pub enum SsTableError {
    #[snafu(context(false), display("SSTable io operation failed"))]
    Io {
        source: io::Error,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("{reason}"))]
    Validation {
        reason: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("{reason}"))]
    Corruption {
        reason: String,
        backtrace: snafu::Backtrace,
    },
}

pub struct SsTable {
    path: PathBuf,
    filter: filter::Filter,
    index: index::Index,
    footer: Footer,
    file: fs::File,
}

impl SsTable {
    pub fn open(path: impl Into<path::PathBuf>) -> Result<Self, SsTableError> {
        let path = path.into();
        let mut file = fs::File::open(&path)?;

        file.seek(SeekFrom::End(-(FOOTER_TAIL_SIZE as i64)))?;
        let footer_tail = FooterTail::decode(&mut file)?;
        footer_tail.validate()?;

        file.seek(SeekFrom::End(-(footer_tail.footer_size as i64)))?;
        let footer = Footer::decode(&mut file)?;

        file.seek(SeekFrom::Start(footer.filter_offset))?;
        let filter = filter::Filter::decode(&mut file)?;

        file.seek(SeekFrom::Start(footer.index_offset))?;
        let index = index::Index::decode(&mut file)?;

        Ok(Self {
            path,
            filter,
            index,
            footer,
            file,
        })
    }

    /// Contains checks if the given key is present in the SSTable. This check
    /// can guarantee that a key is not present, but may return false positives.
    pub fn contains(&self, key: &Key) -> bool {
        if self
            .index
            .0
            .last()
            .is_none_or(|entry| key > &entry.last_key)
        {
            return false;
        }
        self.filter.contains(key.as_ref())
    }

    /// Returns an iterator over the key-value pairs in the SSTable.
    pub fn kv_iter<'a>(&'a self) -> iterator::SstIter<'a> {
        iterator::SstIter::new(self)
    }
}

#[derive(Default, Clone, Copy, Debug, PartialEq)]
pub struct SstBuildOpts {
    pub block_opts: block::BlockBuilderOpts,
    pub filter_opts: filter::FilterOpts,
}

#[derive(Snafu, Debug)]
pub enum SstBuilderError {
    #[snafu(context(false), display("SSTable io operation failed"))]
    Io {
        source: io::Error,
        backtrace: snafu::Backtrace,
    },
    #[snafu(context(false), display("Error building SSTable block"))]
    BlockBuild {
        source: block::BlockBuilderError,
        backtrace: snafu::Backtrace,
    },
    #[snafu(whatever, display("{message}"))]
    Whatever {
        message: String,
        #[snafu(source(from(Box<dyn std::error::Error>, Some)))]
        source: Option<Box<dyn std::error::Error>>,
    },
}

pub struct SstBuilder {
    writer: io::BufWriter<fs::File>,
    filter_offset: u64,
    filter: filter::FilterBuilder,
    index: index::IndexBuilder,
    block: block::BlockBuilder,
    last_key: Option<Key>,
    opts: SstBuildOpts,
}

impl SstBuilder {
    pub fn new(path: impl AsRef<path::Path>, opts: SstBuildOpts) -> Result<Self, SstBuilderError> {
        Ok(Self {
            writer: io::BufWriter::new(
                fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)?,
            ),
            filter_offset: 0,
            filter: filter::FilterBuilder::new(opts.filter_opts),
            index: index::IndexBuilder::new(),
            block: block::BlockBuilder::new(opts.block_opts),
            last_key: None,
            opts,
        })
    }

    /// Adds a key-value pair to the SSTable.
    pub fn add(&mut self, key: &Key, value: &Value) -> Result<(), SstBuilderError> {
        self.filter.insert(key.as_ref());
        self.last_key = Some(key.clone());
        if self.block.add_kv(key, value)? {
            self.flush_block()?;
        }
        Ok(())
    }

    /// Finalizes the SSTable by writing it to the sink and flushing the writer.
    pub fn finish(mut self) -> Result<(), SstBuilderError> {
        self.flush_block()?;
        let mut footer = Footer {
            filter_offset: self.filter_offset,
            filter_size: 0,
            index_offset: 0,
            index_size: 0,
        };
        let index = self.index.finish();
        let filter = self.filter.finish();
        footer.filter_size = filter.encode(&mut self.writer)? as u32;
        footer.index_offset = footer.filter_offset + footer.filter_size as u64;
        footer.index_size = index.encode(&mut self.writer)? as u32;
        footer.encode(&mut self.writer)?;
        FooterTail::new().encode(&mut self.writer)?;
        self.writer.flush()?;
        self.writer.get_ref().sync_all()?;
        Ok(())
    }

    /// Flushes the current block to the file and records it in the index.
    fn flush_block(&mut self) -> Result<(), SstBuilderError> {
        if self.block.is_empty() {
            return Ok(());
        }
        let Some(last_key) = &self.last_key else {
            whatever!("no last key stored on a non-empty block")
        };
        let builder = std::mem::replace(
            &mut self.block,
            block::BlockBuilder::new(self.opts.block_opts),
        );
        let block = builder.finish()?;
        let written = block.encode(&mut self.writer)?;
        self.filter_offset += written as u64;
        self.index.add_block(written as u32, last_key);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::testutils::*;
    use super::*;
    use crate::testutils::{TestResult, generate_rand_bytes, key, seeded_rng, value};
    use tempfile::tempdir;

    #[test]
    fn sstable_encoding() -> TestResult<()> {
        let dir = tempdir().expect("create tmp dir");
        let tmp_sst = dir.path().join("tempdata.sst");
        let opts = SstBuildOpts::default();
        let mut builder = SstBuilder::new(&tmp_sst, opts)?;
        builder.add(&key(b"key1"), &value(b"value1"))?;
        builder.add(&key(b"key2"), &value(b"value2"))?;
        builder.finish()?;

        let sstable = SsTable::open(&tmp_sst)?;
        assert!(!sstable.index.0.is_empty());
        assert!(sstable.footer.filter_size > 0);
        assert!(sstable.footer.index_size > 0);
        assert!(sstable.footer.filter_offset > 0);
        assert!(sstable.footer.index_offset > 0);
        Ok(())
    }

    #[test]
    fn contains() -> TestResult<()> {
        let (mut rng, _) = seeded_rng(None);
        let SstFixture {
            dir: _,
            filepath: _,
            sstable,
            entries,
        } = create_random_sstable(100, &mut rng)?;
        for (key, _) in entries {
            assert!(sstable.contains(&key));
        }
        let non_existing_key = key(&generate_rand_bytes(10, &mut rng));
        assert!(!sstable.contains(&non_existing_key));
        Ok(())
    }
}
