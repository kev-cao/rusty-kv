use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use std::borrow::Borrow;
use std::io::{self};

use crate::storage::encoding::Codable;
use crate::storage::kv::Key;

pub const BLOCK_MAX_SIZE: u32 = 4 * 1024; // 4KB

pub struct SsTableIndexEntry {
    pub last_key: Key,
    pub block_offset: u64,
    pub block_size: u32,
}

impl Codable for SsTableIndexEntry {
    type EncodeErr = io::Error;
    type DecodeErr = io::Error;

    fn encode(&self, writer: &mut impl io::Write) -> Result<usize, Self::EncodeErr> {
        let key_length = self.last_key.encode(writer)?;
        writer.write_u64::<LittleEndian>(self.block_offset)?;
        writer.write_u32::<LittleEndian>(self.block_size)?;
        Ok(key_length + size_of_val(&self.block_offset) + size_of_val(&self.block_size))
    }

    fn decode(reader: &mut impl io::Read) -> Result<Self, Self::DecodeErr> {
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
    type EncodeErr = io::Error;
    type DecodeErr = io::Error;

    fn encode(&self, writer: &mut impl io::Write) -> Result<usize, Self::EncodeErr> {
        let mut written = size_of::<u32>();
        writer.write_u32::<LittleEndian>(self.0.len() as u32)?;
        for entry in self {
            written += entry.encode(writer)?;
        }
        Ok(written)
    }

    fn decode(reader: &mut impl io::Read) -> Result<Self, Self::DecodeErr> {
        let len = reader.read_u32::<LittleEndian>()?;
        let mut ret = Self::new();
        for _ in 0..len {
            let entry = SsTableIndexEntry::decode(reader)?;
            ret.0.push(entry);
        }
        Ok(ret)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SsTableIndexOpts {
    pub block_max_size: u32,
}

impl Default for SsTableIndexOpts {
    fn default() -> Self {
        Self {
            block_max_size: BLOCK_MAX_SIZE,
        }
    }
}

impl Codable for SsTableIndexOpts {
    type EncodeErr = io::Error;
    type DecodeErr = io::Error;

    fn encode(&self, writer: &mut impl io::Write) -> Result<usize, Self::EncodeErr> {
        writer.write_u32::<LittleEndian>(self.block_max_size)?;
        Ok(size_of_val(&self.block_max_size))
    }

    fn decode(reader: &mut impl io::Read) -> Result<Self, Self::DecodeErr> {
        let block_max_size = reader.read_u32::<LittleEndian>()?;
        Ok(Self { block_max_size })
    }
}

pub struct SsTableIndexBuilder {
    index: SsTableIndex,
    current_block: Option<SsTableIndexEntry>,
    opts: SsTableIndexOpts,
}

impl SsTableIndexBuilder {
    pub fn new(opts: SsTableIndexOpts) -> Self {
        Self {
            index: SsTableIndex::new(),
            current_block: None,
            opts,
        }
    }

    /// Records the addition of a new key to the index. Key represents the
    /// logical key being added, while enc_kv_len represents the length of the
    /// on-disk encoded key and value pair.
    pub fn add_key(&mut self, key: &Key, enc_kv_len: usize) {
        let needs_new_block = match &self.current_block {
            None => true,
            Some(block) => block
                .block_size
                .checked_add(enc_kv_len as u32)
                .is_none_or(|l| l > self.opts.block_max_size),
        };

        if needs_new_block {
            let block_offset = match self.current_block.take() {
                None => 0,
                Some(block) => {
                    let offset = block.block_offset + block.block_size as u64;
                    self.index.0.push(block);
                    offset
                }
            };
            self.current_block = Some(SsTableIndexEntry {
                last_key: Key::default(),
                block_size: 0,
                block_offset,
            });
        }

        let last_entry = self
            .current_block
            .as_mut()
            .expect("index should be non-empty as this point");
        last_entry.last_key = key.clone();
        last_entry.block_size += enc_kv_len as u32;
    }

    /// Finalizes the index and returns the built SSTableIndex.
    pub fn finish(mut self) -> SsTableIndex {
        if let Some(block) = self.current_block.take() {
            self.index.0.push(block);
        }
        self.index
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutils::{generate_rand_bytes, key, seeded_rng};

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
    fn sstable_index_key_block_offset() {
        const BLOCK_SIZE: u32 = 16;
        let mut builder = SsTableIndexBuilder::new(SsTableIndexOpts {
            block_max_size: BLOCK_SIZE,
        });
        for i in 0..5 {
            // The encoding of a key is 2 bytes for the length + the length of
            // the key itself. So we choose a key of length 6, which allows for
            // 2 keys in each block.
            let add_key = key(&[b'a' + i; 6]);
            let encode_len = add_key.encode(&mut std::io::sink()).unwrap();
            builder.add_key(&add_key, encode_len);
        }

        let index = builder.finish();
        for i in 0..5 {
            let search_key = key(&[b'a' + i; 6]);
            let (block, _) = index.key_block(&search_key).unwrap_or_else(|| {
                panic!("failed to find block offset for key {}", (b'a' + i) as char)
            });
            assert_eq!(
                block.block_offset,
                (i / 2) as u64 * BLOCK_SIZE as u64,
                "key {}",
                (b'a' + i) as char
            );
        }
    }
}
