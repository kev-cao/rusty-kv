use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use std::borrow::Borrow;
use std::io::{self};

use crate::storage::encoding::Codable;
use crate::storage::kv::Key;
use crate::storage::sstable::block::SsTableBlock;

pub struct SsTableIndexEntry {
    pub last_key: Key,
    pub block_offset: u64,
    pub block_size: u32,
}

impl Codable for SsTableIndexEntry {
    fn encode(&self, writer: &mut impl io::Write) -> Result<usize, io::Error> {
        let key_length = self.last_key.encode(writer)?;
        writer.write_u64::<LittleEndian>(self.block_offset)?;
        writer.write_u32::<LittleEndian>(self.block_size)?;
        Ok(key_length + size_of_val(&self.block_offset) + size_of_val(&self.block_size))
    }

    fn decode(reader: &mut impl io::Read) -> Result<Self, io::Error> {
        Ok(Self {
            last_key: Key::decode(reader)?,
            block_offset: reader.read_u64::<LittleEndian>()?,
            block_size: reader.read_u32::<LittleEndian>()?,
        })
    }
}

pub struct SsTableIndex(pub Vec<SsTableIndexEntry>);

impl<'a> IntoIterator for &'a SsTableIndex {
    type Item = &'a SsTableIndexEntry;
    type IntoIter = std::slice::Iter<'a, SsTableIndexEntry>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl IntoIterator for SsTableIndex {
    type Item = SsTableIndexEntry;
    type IntoIter = std::vec::IntoIter<SsTableIndexEntry>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl SsTableIndex {
    pub fn new() -> SsTableIndex {
        Self(vec![])
    }

    /// Returns the index entry that may contain the given key. If the key is
    /// guaranteed to not exist in the index, as it lays beyond the last key in
    /// the index, returns None. Otherwise, returns the index entry that may
    /// contain the key. Also returns the index of the entry in the index vector.
    pub fn key_block<Q>(&self, key: &Q) -> Option<(&SsTableIndexEntry, usize)>
    where
        Key: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        match self
            .0
            .binary_search_by(|entry| entry.last_key.borrow().cmp(key))
        {
            Ok(idx) => Some((&self.0[idx], idx)),
            Err(idx) => self.0.get(idx).map(|entry| (entry, idx)),
        }
    }
}

impl Codable for SsTableIndex {
    fn encode(&self, writer: &mut impl io::Write) -> Result<usize, io::Error> {
        let mut written = size_of::<u32>();
        writer.write_u32::<LittleEndian>(self.0.len() as u32)?;
        for entry in self {
            written += entry.encode(writer)?;
        }
        Ok(written)
    }

    fn decode(reader: &mut impl io::Read) -> Result<Self, io::Error> {
        let len = reader.read_u32::<LittleEndian>()?;
        let mut ret = Self::new();
        for _ in 0..len {
            let entry = SsTableIndexEntry::decode(reader)?;
            ret.0.push(entry);
        }
        Ok(ret)
    }
}

pub struct SsTableIndexBuilder {
    index: SsTableIndex,
    current_offset: u64,
}

impl SsTableIndexBuilder {
    pub fn new() -> Self {
        Self {
            index: SsTableIndex::new(),
            current_offset: 0,
        }
    }

    /// Adds a new block to the index. Last key refers to the last key in the
    /// block being added.
    pub fn add_block(&mut self, block: &SsTableBlock, last_key: &Key) {
        let entry = SsTableIndexEntry {
            last_key: last_key.clone(),
            block_offset: self.current_offset,
            block_size: block.block_size() as u32,
        };
        self.current_offset += entry.block_size as u64;
        self.index.0.push(entry);
    }

    /// Finalizes the index and returns the built SSTableIndex.
    pub fn finish(self) -> SsTableIndex {
        self.index
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        storage::sstable::block::SsTableBlockBuilder,
        testutils::{TestResult, generate_rand_bytes, key, seeded_rng, value},
    };

    #[test]
    fn sstable_index_encoding_roundtrip() {
        let (mut rng, _) = seeded_rng(None);
        const KEY_LEN: usize = 16;
        let key = key(&generate_rand_bytes(KEY_LEN, &mut rng));

        let index_entry = SsTableIndexEntry {
            last_key: key,
            block_offset: 16,
            block_size: 32,
        };

        let mut buf = vec![];
        index_entry
            .encode(&mut buf)
            .expect("failed to encode index entry");
        let decoded_entry =
            SsTableIndexEntry::decode(&mut buf.as_slice()).expect("failed to decode index entry");

        assert_eq!(index_entry.last_key, decoded_entry.last_key);
        assert_eq!(index_entry.block_offset, decoded_entry.block_offset);
        assert_eq!(index_entry.block_size, decoded_entry.block_size);
    }

    #[test]
    fn sstable_index_key_block_offset() -> TestResult<()> {
        let mut index_builder = SsTableIndexBuilder::new();
        let mut block_builder = SsTableBlockBuilder::new(Default::default());

        // Loop must end on an odd number so that we flush the last block.
        for i in 0..=5 {
            let k = key(&[b'a' + i; 6]);
            let v = value(&[0u8; 1]);
            block_builder.add_kv(&k, &v)?;
            // We'll have two keys per block.
            if i % 2 == 1 {
                let block = block_builder.finish()?;
                index_builder.add_block(&block, &k);
                block_builder = SsTableBlockBuilder::new(Default::default());
            }
        }
        let index = index_builder.finish();
        let block_offsets = index
            .0
            .iter()
            .map(|entry| entry.block_offset)
            .collect::<Vec<_>>();

        for i in 0..=5 {
            let search_key = key(&[b'a' + i; 6]);
            let (block, _) = index.key_block(&search_key).unwrap_or_else(|| {
                panic!("failed to find block offset for key {}", (b'a' + i) as char)
            });
            assert_eq!(
                block.block_offset,
                block_offsets[i as usize / 2],
                "key {}",
                (b'a' + i) as char
            );
        }
        Ok(())
    }
}
