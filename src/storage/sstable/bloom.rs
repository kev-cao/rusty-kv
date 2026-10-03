use std::f64::consts::LN_2;
use std::io;

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use xxhash_rust::{const_xxh3::xxh3_64 as const_xxh3, xxh3::xxh3_64_with_seed as xxh3_64};

use crate::storage::encoding::Codable;

const FILTER_SIZE: u32 = 12 * 1024; // 12 KiB
const NUM_HASHES: u32 = 7;

pub struct BloomFilter {
    bits: Vec<u8>,
    /// size is a u32, which can be safely converted to usize on all platforms.
    size: u32,
    num_hashes: u32,
}

impl BloomFilter {
    pub fn new(size: u32, num_hashes: u32) -> Self {
        Self {
            bits: vec![0; size as usize],
            size,
            num_hashes,
        }
    }

    pub fn contains(&self, key: &[u8]) -> bool {
        for n in 0..self.num_hashes {
            let hash = xxh3_64(key, create_seed(n));
            let bit_idx = (hash % (self.size as u64 * 8)) as usize;
            let byte_idx = bit_idx / 8;
            let bit = 1 << (bit_idx % 8);
            if self.bits[byte_idx] & bit == 0 {
                return false;
            }
        }
        true
    }

    pub fn len(&self) -> usize {
        self.size as usize
    }
}

impl AsRef<[u8]> for BloomFilter {
    fn as_ref(&self) -> &[u8] {
        self.bits.as_ref()
    }
}

impl Codable for BloomFilter {
    type EncodeErr = io::Error;
    type DecodeErr = io::Error;

    fn encode(&self, writer: &mut impl io::Write) -> Result<usize, Self::EncodeErr> {
        writer.write_u32::<LittleEndian>(self.bits.len() as u32)?;
        writer.write_u32::<LittleEndian>(self.num_hashes)?;
        writer.write_all(self.as_ref())?;
        Ok(size_of::<u32>() + size_of::<u32>() + self.bits.len())
    }

    fn decode(reader: &mut impl io::Read) -> Result<Self, Self::DecodeErr> {
        let size = reader.read_u32::<LittleEndian>()?;
        let num_hashes = reader.read_u32::<LittleEndian>()?;
        let mut bits = vec![0; size as usize];
        reader.read_exact(&mut bits)?;
        Ok(Self {
            bits,
            size,
            num_hashes,
        })
    }
}

/// Generates a seed for the hash function based on the given number. This
/// provides a deterministic way to generate seeds for the hash functions used
/// in the filter.
fn create_seed(n: u32) -> u64 {
    const_xxh3(&n.to_le_bytes())
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BloomFilterOpts {
    pub filter_size: u32,
    pub num_hashes: u32,
}

impl Default for BloomFilterOpts {
    fn default() -> Self {
        // 12 KiB bloom filter size and 7 hashes can accomodate 10k keys at a 1%
        // false positive rate.
        Self {
            filter_size: FILTER_SIZE,
            num_hashes: NUM_HASHES,
        }
    }
}

impl Codable for BloomFilterOpts {
    type EncodeErr = io::Error;
    type DecodeErr = io::Error;

    fn encode(&self, writer: &mut impl io::Write) -> Result<usize, Self::EncodeErr> {
        writer.write_u32::<LittleEndian>(self.filter_size)?;
        writer.write_u32::<LittleEndian>(self.num_hashes)?;
        Ok(size_of::<u32>() + size_of::<u32>())
    }

    fn decode(reader: &mut impl io::Read) -> Result<Self, Self::DecodeErr> {
        let filter_size = reader.read_u32::<LittleEndian>()?;
        let num_hashes = reader.read_u32::<LittleEndian>()?;
        Ok(Self {
            filter_size,
            num_hashes,
        })
    }
}

pub struct BloomFilterBuilder {
    filter: BloomFilter,
    opts: BloomFilterOpts,
}

impl BloomFilterBuilder {
    pub fn new(opts: BloomFilterOpts) -> Self {
        Self {
            filter: BloomFilter::new(opts.filter_size, opts.num_hashes),
            opts,
        }
    }

    /// Inserts a key to be tracked by the bloom filter.
    pub fn insert(&mut self, key: &[u8]) {
        for n in 0..self.filter.num_hashes {
            let hash = xxh3_64(key, create_seed(n));
            let bit_idx = (hash % (self.filter.size as u64 * 8)) as usize;
            let byte_idx = bit_idx / 8;
            let bit = 1 << (bit_idx % 8);
            self.filter.bits[byte_idx] |= bit;
        }
    }

    /// Finalizes the bloom filter and returns it.
    pub fn finish(self) -> BloomFilter {
        self.filter
    }
}

/// Optimal bit array size (m) for the target false positive rate.
fn optimal_bit_count(expected_num_items: u32, false_positive_rate: f64) -> usize {
    let n = expected_num_items as f64;
    let p = false_positive_rate;
    let m = -(n * p.ln()) / (LN_2 * LN_2);
    m.ceil() as usize
}

/// Optimal number of hash functions (k) given m and n.
fn optimal_hash_count(bit_count: usize, expected_num_items: u32) -> u32 {
    let m = bit_count as f64;
    let n = expected_num_items as f64;
    let k = (m / n) * LN_2;
    k.round().max(1.0) as u32
}

#[cfg(test)]
mod tests {
    use crate::testutils::{generate_rand_bytes, seeded_rng};

    use super::*;

    #[test]
    fn bloom_roundtrip() {
        let (mut seeded_rng, _) = seeded_rng(None);
        let mut builder = BloomFilterBuilder::new(BloomFilterOpts::default());

        let keys = (0..100)
            .map(|_| {
                let key = generate_rand_bytes(16, &mut seeded_rng);
                builder.insert(&key);
                assert!(
                    builder.filter.contains(&key),
                    "bloom filter does not contain inserted key"
                );
                key
            })
            .collect::<Vec<_>>();

        let filter = builder.finish();
        for key in keys.iter() {
            assert!(
                filter.contains(key),
                "bloom filter does not contain previously inserted key"
            );
        }
    }

    #[test]
    fn bloom_nonexistence() {
        let (mut seeded_rng, _) = seeded_rng(None);
        let mut builder = BloomFilterBuilder::new(BloomFilterOpts::default());

        for _ in 0..100 {
            let mut key = generate_rand_bytes(16, &mut seeded_rng);
            key.append(&mut Vec::from(b"batch_a"));
            builder.insert(&key);
        }

        let filter = builder.finish();

        // Technically, bloom filters can have false positives, but with the
        // chosen parameters the false positive rate should be very low, so we
        // can assert that non-inserted keys are not contained.
        for _ in 0..100 {
            let mut key = generate_rand_bytes(16, &mut seeded_rng);
            key.append(&mut Vec::from(b"batch_b"));
            assert!(
                !filter.contains(&key),
                "bloom filter contains non-inserted key"
            );
        }
    }

    #[test]
    fn bloom_false_positive_rate() {
        let (mut seeded_rng, _) = seeded_rng(None);
        let mut builder = BloomFilterBuilder::new(BloomFilterOpts::default());
        const EXP_NUM_ITEMS: u32 = 10_000;

        for _ in 0..EXP_NUM_ITEMS {
            let key = crate::testutils::generate_rand_bytes(16, &mut seeded_rng);
            builder.insert(&key);
        }

        let filter = builder.finish();

        let mut false_positives = 0;
        for _ in 0..EXP_NUM_ITEMS {
            let key = crate::testutils::generate_rand_bytes(16, &mut seeded_rng);
            if filter.contains(&key) {
                false_positives += 1;
            }
        }

        let false_positive_rate = false_positives as f64 / 10_000.0;
        assert!(
            false_positive_rate < 0.015,
            "bloom filter false positive rate is too high: {}%",
            false_positive_rate * 100.0
        )
    }

    #[test]
    fn bloom_filter_encode_decode() {
        let (mut seeded_rng, _) = seeded_rng(None);
        let mut builder = BloomFilterBuilder::new(BloomFilterOpts::default());

        for _ in 0..100 {
            let key = generate_rand_bytes(16, &mut seeded_rng);
            builder.insert(&key);
        }

        let filter = builder.finish();

        let mut buf = vec![];
        filter
            .encode(&mut buf)
            .expect("failed to encode bloom filter");
        let decoded_filter =
            BloomFilter::decode(&mut buf.as_slice()).expect("failed to decode bloom filter");

        assert_eq!(filter.size, decoded_filter.size);
        assert_eq!(filter.num_hashes, decoded_filter.num_hashes);
        assert_eq!(filter.bits, decoded_filter.bits);
    }
}
