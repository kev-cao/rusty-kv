mod bloom;
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
    sstable::footer::{FOOTER_TAIL_SIZE, SsTableFooter},
};
use crate::storage::{
    kv::{Key, Value},
    sstable::footer::SsTableFooterTail,
};

pub const SST_VERSION: u16 = 1;

#[derive(Snafu, Debug)]
pub enum SsTableError {
    #[snafu(context(false), display("SSTable io operation failed"))]
    IoError {
        source: io::Error,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("SSTableFooter validation failed: {reason}"))]
    ValidationError {
        reason: String,
        backtrace: snafu::Backtrace,
    },
}

pub struct SsTable {
    path: PathBuf,
    filter: bloom::BloomFilter,
    index: index::SsTableIndex,
    opts: SsTableOpts,
    footer: SsTableFooter,
    file: fs::File,
}

impl SsTable {
    pub fn open(path: impl Into<path::PathBuf>) -> Result<Self, SsTableError> {
        let path = path.into();
        let mut file = fs::File::open(&path)?;

        file.seek(SeekFrom::End(-(FOOTER_TAIL_SIZE as i64)))?;
        let footer_tail = SsTableFooterTail::decode(&mut file)?;

        file.seek(SeekFrom::End(-(footer_tail.footer_size as i64)))?;
        let footer = SsTableFooter::decode(&mut file)?;

        file.seek(SeekFrom::Start(footer.filter_offset))?;
        let filter = bloom::BloomFilter::decode(&mut file)?;

        file.seek(SeekFrom::Start(footer.index_offset))?;
        let index = index::SsTableIndex::decode(&mut file)?;

        file.seek(SeekFrom::Start(footer.opts_offset))?;
        let opts = SsTableOpts::decode(&mut file)?;

        Ok(Self {
            path,
            filter,
            index,
            opts,
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
    pub fn kv_iter<'a>(&'a self) -> iterator::SsTableIterator<'a> {
        iterator::SsTableIterator::new(self)
    }
}

#[derive(Default, Clone, Copy, Debug, PartialEq)]
pub struct SsTableOpts {
    pub index_opts: index::SsTableIndexOpts,
    pub filter_opts: bloom::BloomFilterOpts,
}

impl Codable for SsTableOpts {
    type EncodeErr = io::Error;
    type DecodeErr = io::Error;

    fn encode(&self, writer: &mut impl io::Write) -> Result<usize, Self::EncodeErr> {
        let mut written = 0;
        written += self.index_opts.encode(writer)?;
        written += self.filter_opts.encode(writer)?;
        Ok(written)
    }

    fn decode(reader: &mut impl io::Read) -> Result<Self, Self::DecodeErr> {
        let index_opts = index::SsTableIndexOpts::decode(reader)?;
        let filter_opts = bloom::BloomFilterOpts::decode(reader)?;
        Ok(Self {
            index_opts,
            filter_opts,
        })
    }
}

pub struct SsTableBuilder {
    path: PathBuf,
    writer: io::BufWriter<fs::File>,
    filter_offset: u64,
    filter: bloom::BloomFilterBuilder,
    index: index::SsTableIndexBuilder,
    opts: SsTableOpts,
}

impl SsTableBuilder {
    pub fn new(path: impl AsRef<path::Path>, opts: SsTableOpts) -> Result<Self, SsTableError> {
        Ok(Self {
            path: path.as_ref().to_path_buf(),
            writer: io::BufWriter::new(
                fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)?,
            ),
            filter_offset: 0,
            filter: bloom::BloomFilterBuilder::new(opts.filter_opts),
            index: index::SsTableIndexBuilder::new(opts.index_opts),
            opts,
        })
    }

    /// Adds a key-value pair to the SSTable.
    pub fn add(&mut self, key: &Key, value: &Value) -> Result<(), SsTableError> {
        let enc_key_len = key.encode(&mut self.writer)?;
        let enc_val_len = value.encode(&mut self.writer)?;
        self.filter_offset += enc_key_len as u64 + enc_val_len as u64;

        self.filter.insert(key.as_ref());
        self.index
            .add_key(key, enc_key_len.saturating_add(enc_val_len));
        Ok(())
    }

    /// Finalizes the SSTable by writing it to the sink and flushing the writer.
    pub fn finish(mut self) -> Result<(), SsTableError> {
        let mut footer = SsTableFooter {
            filter_offset: self.filter_offset,
            filter_size: 0,
            index_offset: 0,
            index_size: 0,
            opts_offset: 0,
            opts_size: 0,
        };
        let index = self.index.finish();
        let filter = self.filter.finish();
        footer.filter_size = filter.encode(&mut self.writer)? as u32;
        footer.index_offset = footer.filter_offset + footer.filter_size as u64;
        footer.index_size = index.encode(&mut self.writer)? as u32;
        footer.opts_offset = footer.index_offset + footer.index_size as u64;
        footer.opts_size = self.opts.encode(&mut self.writer)? as u32;
        footer.encode(&mut self.writer)?;
        SsTableFooterTail::new().encode(&mut self.writer)?;
        self.writer.flush()?;
        self.writer.get_ref().sync_all()?;
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
        let opts = SsTableOpts::default();
        let mut builder = SsTableBuilder::new(&tmp_sst, opts)?;
        builder.add(&key(b"key1"), &value(b"value1"))?;
        builder.add(&key(b"key2"), &value(b"value2"))?;
        builder.finish()?;

        let sstable = SsTable::open(&tmp_sst)?;
        assert_eq!(SsTableOpts::default(), sstable.opts);
        assert!(!sstable.index.0.is_empty());
        assert!(sstable.footer.filter_size > 0);
        assert!(sstable.footer.index_size > 0);
        assert!(sstable.footer.opts_size > 0);
        assert!(sstable.footer.filter_offset > 0);
        assert!(sstable.footer.index_offset > 0);
        assert!(sstable.footer.opts_offset > 0);
        Ok(())
    }

    #[test]
    fn contains() -> TestResult<()> {
        let (mut rng, _) = seeded_rng(None);
        let SsTableFixture {
            _dir,
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
