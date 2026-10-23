use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use std::io;

use crate::storage::{
    kv::{Key, KeyLen, Value, ValueLen},
    sstable::{CorruptionSnafu, SsTableError},
};

use snafu::prelude::*;

pub const BLOCK_MAX_SIZE: u32 = 4 * 1024; // 4KB
pub const RESTART_INTERVAL: u32 = 16;

/**
 * Structure of a block on disk:
 * +-----------------+---------------+----------------------+
 * | Key-Value Pairs | Restart Array | 4-byte Restart Count |
 * +-----------------+---------------+----------------------+
 *
 * Each key-value pair is encoded as follows:
 * +-------------------+------------------+--------------+-----------+-------+
 * | Shared Prefix Len | Key Delta Length | Value Length | Key Delta | Value |
 * +-------------------+------------------+--------------+-----------+-------+
 * */
#[derive(Debug)]
pub struct Block {
    pub data: Vec<u8>,
    pub restart_array: Vec<u32>,
}

impl Block {
    fn new() -> Self {
        Self {
            data: Vec::new(),
            restart_array: Vec::new(),
        }
    }

    /// Returns the size of the block on disk in bytes.
    pub fn block_size(&self) -> usize {
        self.data.len()
    }

    /// Encodes the block to the given writer. Returns the number of bytes
    /// written.
    pub fn encode(&self, writer: &mut impl io::Write) -> Result<usize, io::Error> {
        let mut written = 0;
        writer.write_all(&self.data)?;
        written += self.data.len();
        writer.write_all(
            &self
                .restart_array
                .iter()
                .flat_map(|&x| x.to_le_bytes())
                .collect::<Vec<u8>>(),
        )?;
        written += self.restart_array.len() * 4;
        writer.write_u32::<LittleEndian>(self.restart_array.len() as u32)?;
        written += std::mem::size_of::<u32>();
        Ok(written)
    }

    /// Decodes a block from the given reader. The size of the block on disk
    /// must be provided, as the block does not contain any metadata about its
    /// size, only the index does.
    pub fn decode(reader: &mut impl io::Read, size: usize) -> Result<Self, SsTableError> {
        let mut disk_data = vec![0u8; size];
        reader.read_exact(&mut disk_data)?;
        let restart_size_offset = size.checked_sub(4).ok_or_else(|| {
            CorruptionSnafu {
                reason: "block size is too small to contain restart count".to_string(),
            }
            .build()
        })?;
        let restart_size = (&disk_data[restart_size_offset..]).read_u32::<LittleEndian>()? as usize;
        let restart_offset = restart_size_offset
            .checked_sub(restart_size * 4)
            .ok_or_else(|| {
                CorruptionSnafu {
                    reason: "block size is too small to contain restart array".to_string(),
                }
                .build()
            })?;
        let restart_array = disk_data[restart_offset..size - 4]
            .chunks_exact(4)
            .map(|mut chunk| chunk.read_u32::<LittleEndian>())
            .collect::<Result<Vec<u32>, io::Error>>()?;
        let data = disk_data[..restart_offset].to_vec();
        Ok(Self {
            data,
            restart_array,
        })
    }
}

#[derive(Snafu, Debug)]
pub enum BlockBuilderError {
    #[snafu(context(false), display("SSTable io operation failed"))]
    Io {
        source: io::Error,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("SSTable block is full"))]
    BlockFull { backtrace: snafu::Backtrace },
    #[snafu(display("SSTable block is empty"))]
    EmptyBlock { backtrace: snafu::Backtrace },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlockBuilderOpts {
    /// The maximum size of the block in bytes. This serves as a soft-limit;
    /// once the block exceeds or equals this size, the builder will no longer
    /// accepts new KV pairs.
    pub max_size: u32,
    /// Number of KV pairs between restart points.
    pub restart_interval: u32,
}

impl Default for BlockBuilderOpts {
    fn default() -> Self {
        Self {
            max_size: BLOCK_MAX_SIZE,
            restart_interval: RESTART_INTERVAL,
        }
    }
}

pub struct BlockBuilder {
    block: Block,
    last_restart_key: Option<Key>,
    curr_restart_len: u32,
    opts: BlockBuilderOpts,
}

impl BlockBuilder {
    pub fn new(opts: BlockBuilderOpts) -> Self {
        let mut block = Block::new();
        // We never allow an empty block, so we'll insert the first restart
        // point to keep the logic below simpler.
        block.restart_array.push(0);
        Self {
            block,
            opts,
            last_restart_key: None,
            curr_restart_len: 0,
        }
    }

    /// Adds a key-value pair to the block. Returns true if the block is full
    /// after adding the pair. This assumes that the keys are added in sorted
    /// order.
    pub fn add_kv(&mut self, key: &Key, value: &Value) -> Result<bool, BlockBuilderError> {
        let mut written = self.block.data.len();
        if written >= self.opts.max_size as usize {
            return BlockFullSnafu.fail();
        }
        let mut is_restart_point = false;
        if self.curr_restart_len >= self.opts.restart_interval {
            is_restart_point = true;
            self.block.restart_array.push(written as u32);
            self.curr_restart_len = 0;
            self.last_restart_key = None;
        } else if self.last_restart_key.is_none() {
            // Handle the case where this is the first key being added to the
            // block. We set it to None first for encoding.
            is_restart_point = true;
            self.last_restart_key = None;
        }
        written += encode_kv_pair(
            &mut self.block.data,
            self.last_restart_key.as_ref(),
            key,
            value,
        )?;
        if is_restart_point {
            self.last_restart_key = Some(key.clone());
        }
        self.curr_restart_len += 1;
        Ok(written >= self.opts.max_size as usize)
    }

    /// Checks if the block built so far is empty.
    pub fn is_empty(&self) -> bool {
        self.block.data.is_empty()
    }

    /// Finalizes the block and returns the built `SsTableBlock`.
    pub fn finish(self) -> Result<Block, BlockBuilderError> {
        if self.block.data.is_empty() {
            return EmptyBlockSnafu.fail();
        }
        Ok(self.block)
    }
}

/// Encodes a key-value pair to the given writer of an SSTable block. The
/// restart key is used to determine the prefix length of the key. If the
/// restart key is `None`, the key is assumed to be a restart point.
pub fn encode_kv_pair(
    writer: &mut impl io::Write,
    restart_key: Option<&Key>,
    key: &Key,
    value: &Value,
) -> Result<usize, io::Error> {
    let prefix_len = if let Some(restart_key) = restart_key {
        let mut len = 0;
        for (a, b) in restart_key.as_ref().iter().zip(key.as_ref().iter()) {
            if a != b {
                break;
            }
            len += 1;
        }
        len as u16
    } else {
        0
    };
    let mut written = 0;
    writer.write_u16::<LittleEndian>(prefix_len)?;
    written += std::mem::size_of::<KeyLen>();
    writer.write_u16::<LittleEndian>(key.len() - prefix_len)?;
    written += std::mem::size_of::<KeyLen>();
    writer.write_u32::<LittleEndian>(value.len())?;
    written += std::mem::size_of::<ValueLen>();
    writer.write_all(&key.as_ref()[prefix_len as usize..])?;
    written += key.len() as usize - prefix_len as usize;
    writer.write_all(&value.0)?;
    written += value.len() as usize;
    Ok(written)
}

/// Decodes a key-value pair from an SSTable block. If no restart key is
/// provided, the key is assumed to be a restart point. The amount of bytes read
/// is also returned.
/// NB: This assumes the reader is positioned at the start of a serialized
/// key-value pair in an SSTable block.
pub fn decode_kv_pair(
    reader: &mut impl io::Read,
    restart_key: Option<&Key>,
) -> Result<(Key, Value, usize), io::Error> {
    let (key, value_len, mut bytes_read) = decode_key(reader, restart_key)?;
    let mut value = vec![0u8; value_len as usize];
    reader.read_exact(&mut value)?;
    bytes_read += value_len as usize;
    Ok((key, Value(value), bytes_read))
}

/// Decodes just the key from an SSTable block. If no restart key is provided,
/// the key is assumed to be a restart point. Returns the key, the length of the
/// value, and the amount of bytes read.
/// NB: This assumes the reader is positioned at the start of a serialized
/// key-value pair in an SSTable block.
pub fn decode_key(
    reader: &mut impl io::Read,
    restart_key: Option<&Key>,
) -> Result<(Key, ValueLen, usize), io::Error> {
    let (prefix_len, key_delta_len, value_len, mut bytes_read) = decode_kv_meta(reader)?;
    let mut key_delta = vec![0u8; key_delta_len as usize];
    reader.read_exact(&mut key_delta)?;
    bytes_read += key_delta_len as usize;
    let Some(restart_key) = restart_key else {
        return Ok((Key(key_delta), value_len, bytes_read));
    };
    let mut full_key = vec![0; prefix_len as usize + key_delta.len()];
    if prefix_len > 0 {
        full_key[..prefix_len as usize]
            .copy_from_slice(&restart_key.as_ref()[..prefix_len as usize]);
    }
    full_key[prefix_len as usize..].copy_from_slice(&key_delta);
    Ok((Key(full_key), value_len, bytes_read))
}

/// Decodes only the metadata of a key-value pair from an SSTable block. This is
/// useful for skipping over key-value pairs without reading the entire key and
/// value into memory. Returns a tuple of (prefix_len, key_delta_len,
/// value_len, bytes_read).
/// NB: This assumes the reader is positioned at the start of a serialized
/// key-value pair in an SSTable block.
pub fn decode_kv_meta(
    reader: &mut impl io::Read,
) -> Result<(KeyLen, KeyLen, ValueLen, usize), io::Error> {
    let prefix_len = reader.read_u16::<LittleEndian>()?;
    let key_delta_len = reader.read_u16::<LittleEndian>()?;
    let value_len = reader.read_u32::<LittleEndian>()?;
    Ok((
        prefix_len,
        key_delta_len,
        value_len,
        std::mem::size_of::<KeyLen>() * 2 + std::mem::size_of::<ValueLen>(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::kv::Key;
    use crate::testutils::{TestResult, key, value};
    use rstest::rstest;
    use std::assert_matches;

    #[rstest]
    #[case::no_restart(key(b"apple"), None)]
    #[case::restart_shares_some_prefix(key(b"application"), Some(key(b"apple")))]
    #[case::restart_is_prefix_substring(key(b"apple jacks"), Some(key(b"apple")))]
    #[case::restart_shares_no_prefix(key(b"banana"), Some(key(b"apple")))]
    #[case::restart_is_empty(key(b"apple"), Some(key(b"")))]
    #[case::restart_is_none_new_key_is_empty(key(b""), None)]
    fn encode_decode_kv_pair(
        #[case] new_key: Key,
        #[case] restart_key: Option<Key>,
    ) -> TestResult<()> {
        let value = value(b"some value");
        let mut encoded = Vec::new();
        let bytes_written = encode_kv_pair(&mut encoded, restart_key.as_ref(), &new_key, &value)
            .expect("encoding kv pair should succeed");
        assert_ne!(bytes_written, 0, "encoding kv pair should write some bytes");
        let (decoded_key, decoded_value, bytes_read) =
            decode_kv_pair(&mut encoded.as_slice(), restart_key.as_ref())
                .expect("decoding kv pair should succeed");
        assert_eq!(
            decoded_key, new_key,
            "decoded key should match the original key"
        );
        assert_eq!(
            decoded_value, value,
            "decoded value should match the original value"
        );
        assert_eq!(
            bytes_written, bytes_read,
            "bytes written and read should match"
        );
        Ok(())
    }

    #[rstest]
    #[case::size_one_interval(1, vec![key(b"apple"), key(b"banana"), key(b"cherry")], 3)]
    #[case::multi_key_restarts( 3, vec![
        key(b"apple"), key(b"banana"), key(b"cherry"),
        key(b"date"), key(b"elderberry"), key(b"fig"),
        key(b"grape"),
    ], 3)]
    #[case::singleton_restart(10, vec![
        key(b"apple"), key(b"banana"), key(b"cherry"), key(b"date"), key(b"elderberry"),
    ], 1)]
    fn restart_intervals(
        #[case] restart_interval: u32,
        #[case] keys: Vec<Key>,
        #[case] exp_n_restarts: usize,
    ) -> TestResult<()> {
        let opts = BlockBuilderOpts {
            max_size: 1024,
            restart_interval,
        };
        let mut builder = BlockBuilder::new(opts);
        // To keep things simple, our value will make up 10 bytes, including its
        // length encoding.
        let val = value(b"10bytes");
        for key in keys {
            builder.add_kv(&key, &val)?;
        }
        assert_eq!(
            builder.block.restart_array.len(),
            exp_n_restarts,
            "number of restart points should match expected"
        );
        Ok(())
    }

    #[test]
    fn fail_on_empty_block() -> TestResult<()> {
        let opts = BlockBuilderOpts {
            max_size: 1024,
            restart_interval: 2,
        };
        let builder = BlockBuilder::new(opts);
        let result = builder.finish();
        assert_matches!(
            result,
            Err(BlockBuilderError::EmptyBlock { .. }),
            "finishing an empty block should return EmptyBlock error"
        );
        Ok(())
    }

    #[test]
    fn fail_on_full_block() -> TestResult<()> {
        let opts = BlockBuilderOpts {
            max_size: 30, // small size to trigger full block
            restart_interval: 2,
        };
        let mut builder = BlockBuilder::new(opts);
        let val = value(b"10bytes");
        builder.add_kv(&key(b"apple"), &val)?;
        builder.add_kv(&key(b"banana"), &val)?;
        assert_matches!(
            builder.add_kv(&key(b"cherry"), &val),
            Err(BlockBuilderError::BlockFull { .. }),
            "adding a key-value pair to a full block should return BlockFull error"
        );
        Ok(())
    }

    #[test]
    fn roundtrip_encode_decode_block() -> TestResult<()> {
        let opts = BlockBuilderOpts {
            max_size: 1024,
            restart_interval: 2,
        };
        let mut builder = BlockBuilder::new(opts);
        let val = value(b"10bytes");
        let keys = vec![key(b"apple"), key(b"banana"), key(b"cherry")];
        for key in &keys {
            builder.add_kv(key, &val)?;
        }
        let block = builder.finish()?;
        let mut encoded_block = Vec::new();
        block
            .encode(&mut encoded_block)
            .expect("encoding block should succeed");
        let decoded_block = Block::decode(&mut encoded_block.as_slice(), encoded_block.len())?;
        assert_eq!(
            block.data, decoded_block.data,
            "decoded block data should match original"
        );
        assert_eq!(
            block.restart_array, decoded_block.restart_array,
            "decoded block restart array should match original"
        );
        Ok(())
    }
}
