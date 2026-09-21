mod bloom;
mod footer;
mod index;
mod iterator;

use snafu::prelude::*;
use std::{
    borrow::Borrow,
    fs,
    io::{self, Seek, SeekFrom, Write},
    os::unix::fs::FileExt,
    path::{self, PathBuf},
};

use crate::storage::{
    collections::KVIterator,
    kv::{Key, Value},
    sstable::footer::SsTableFooterTail,
};
use crate::storage::{
    encoding::Codable,
    sstable::footer::{FOOTER_TAIL_SIZE, SsTableFooter},
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
    pub fn kv_iter<'a>(&'a self) -> SsTableIterator<'a> {
        SsTableIterator::new(self)
    }
}

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

#[derive(Default, Clone, Copy)]
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
    use super::*;
    use crate::testutils::{TestResult, generate_rand_bytes, key, seeded_rng, value};
    use rand::Rng;
    use tempfile::{TempDir, tempdir};

    const MAX_TEST_KEY_LEN: usize = u16::MAX as usize;
    const MAX_TEST_VAL_LEN: usize = u16::MAX as usize;

    #[test]
    fn contains() -> TestResult<()> {
        let (mut rng, _) = seeded_rng(None);
        let (_, sstable, entries) = create_random_sstable(100, &mut rng)?;
        for (key, _) in entries {
            assert!(sstable.contains(&key));
        }
        let non_existing_key = key(&generate_rand_bytes(10, &mut rng));
        assert!(!sstable.contains(&non_existing_key));
        Ok(())
    }

    #[test]
    fn seek_ge() -> TestResult<()> {
        let (mut rng, _) = seeded_rng(None);
        let (_, sstable, entries) = create_random_sstable(100, &mut rng)?;
        let mut iter = sstable.kv_iter();
        for (key, value) in entries {
            assert!(iter.seek_ge(&key)?);
            assert_eq!(iter.key()?, Some(&key));
            assert_eq!(iter.value()?, Some(&value));
        }
        let non_existing_key = key(&generate_rand_bytes(10, &mut rng));
        assert!(!iter.seek_ge(&non_existing_key)?);
        Ok(())
    }

    fn create_random_sstable(
        num_entries: u32,
        mut rng: impl Rng,
    ) -> TestResult<(TempDir, SsTable, Vec<(Key, Value)>)> {
        let dir = tempdir().expect("create tmp dir");
        let tmp_sst = dir.path().join("tempdata.sst");
        let opts = SsTableOpts::default();
        let mut builder = SsTableBuilder::new(&tmp_sst, opts)?;

        let mut entries = Vec::new();
        // Inserting data into an SSTable must always be in sorted order, so we
        // generate random keys but prefix them with a monotonically increasing
        // number to ensure they are sorted.
        for (prefix, _) in (0..num_entries).enumerate() {
            let key_len = rng.random_range(0..=MAX_TEST_KEY_LEN - size_of_val(&num_entries));
            let val_len = rng.random_range(0..=MAX_TEST_VAL_LEN);
            let key = key(&[
                &prefix.to_be_bytes(),
                generate_rand_bytes(key_len, &mut rng).as_slice(),
            ]
            .concat());
            let value = value(&generate_rand_bytes(val_len, &mut rng));
            builder.add(&key, &value)?;
            entries.push((key, value));
        }
        builder.finish()?;

        let sstable = SsTable::open(&tmp_sst)?;
        Ok((dir, sstable, entries))
    }
}
