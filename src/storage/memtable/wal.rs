use crate::storage::encoding;

use crc32fast::Hasher;
use std::{
    fs,
    io::{self, BufRead, Read, Write},
    path::{self, PathBuf},
};

type KeyLen = u16;
type ValueLen = u32;
type KVs = Vec<(Vec<u8>, Vec<u8>)>;

#[derive(thiserror::Error, Debug)]
pub enum WalError {
    #[error("WAL io operation failed")]
    IOError(#[from] io::Error),
    #[error(transparent)]
    CodecError(#[from] encoding::CodecError),
    #[error("invalid checksum detected")]
    InvalidChecksum,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum SyncMode {
    Sync,
    Async,
}

pub trait Wal {
    /// Appends a key-value pair to the WAL.
    fn append(&mut self, key: &[u8], value: &[u8]) -> Result<(), WalError>;
    /// Synchronously forces the OS to flush all cached data to the physical
    /// disk, ensuring durability. Only meaningful in Async mode.
    fn sync(&mut self) -> Result<(), WalError>;
}

pub struct FileWal {
    path: PathBuf,
    file: io::BufWriter<fs::File>,
    mode: SyncMode,
    encode_buffer: Vec<u8>,
}

impl FileWal {
    /// Creates a new WAL file at the specified path. This will fail if a file
    /// already exists at that path.
    pub fn new(path: impl AsRef<path::Path>, mode: SyncMode) -> Result<Self, WalError> {
        Ok(Self {
            path: path.as_ref().to_path_buf(),
            file: io::BufWriter::new(
                fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)?,
            ),
            mode,
            encode_buffer: Vec::new(),
        })
    }

    /// Opens an existing WAL file at the specified path for appending. This
    /// will fail if no fle exists at the path.
    pub fn open(path: impl AsRef<path::Path>, mode: SyncMode) -> Result<Self, WalError> {
        Ok(Self {
            path: path.as_ref().to_path_buf(),
            file: io::BufWriter::new(fs::OpenOptions::new().append(true).open(path)?),
            mode,
            encode_buffer: Vec::new(),
        })
    }

    /// Recovers all key-value pairs from the WAL file in the order in which
    /// they were appended.
    pub fn recover(path: impl AsRef<path::Path>) -> Result<KVs, WalError> {
        let file = fs::File::open(path)?;
        read_kvs(file)
    }

    /// Deletes the WAL file from disk. This is typically called after a
    /// successful flush of the memtable to disk, as the WAL is no longer needed
    /// once its contents have been safely persisted.
    pub fn delete(path: impl AsRef<path::Path>) -> Result<(), WalError> {
        fs::remove_file(path)?;
        Ok(())
    }

    /// Closes the current WAL and returns its path for later deletion. After
    /// calling this, the WAL can no longer accept writes.
    pub fn close(mut self) -> Result<PathBuf, WalError> {
        self.sync()?;
        Ok(self.path)
    }
}

impl Wal for FileWal {
    fn append(&mut self, key: &[u8], value: &[u8]) -> Result<(), WalError> {
        write_kv(&mut self.file, &mut self.encode_buffer, key, value)?;
        self.file.flush()?;
        if self.mode == SyncMode::Sync {
            self.sync()?;
        }
        Ok(())
    }

    fn sync(&mut self) -> Result<(), WalError> {
        self.file.flush()?;
        self.file.get_ref().sync_all()?;
        Ok(())
    }
}

pub struct MemWal {
    bytes: Vec<u8>,
    encode_buffer: Vec<u8>,
}

impl MemWal {
    /// Creates a new in-memory WAL instance. This is primarily intended for
    /// testing.
    pub fn new() -> Self {
        Self {
            bytes: Vec::new(),
            encode_buffer: Vec::new(),
        }
    }

    /// Recovers all key-value pairs from the in-memory WAL in the order in
    /// which they were appended.
    pub fn recover(bytes: &[u8]) -> Result<KVs, WalError> {
        read_kvs(bytes)
    }
}

impl Wal for MemWal {
    fn append(&mut self, key: &[u8], value: &[u8]) -> Result<(), WalError> {
        write_kv(&mut self.bytes, &mut self.encode_buffer, key, value)
    }

    fn sync(&mut self) -> Result<(), WalError> {
        Ok(())
    }
}

/// Writes a key-value pair to the provided WAL sink.
fn write_kv(
    sink: &mut impl Write,
    buf: &mut Vec<u8>,
    key: &[u8],
    value: &[u8],
) -> Result<(), WalError> {
    buf.clear();
    encoding::encode_key(key, buf)?;
    encoding::encode_value(value, buf)?;
    sink.write_all(buf)?;

    let checksum = {
        let mut hasher = Hasher::new();
        hasher.update(buf);
        hasher.finalize().to_le_bytes()
    };

    sink.write_all(&checksum)?;
    Ok(())
}

/// Reads all key-value pairs from the provided WAL source.
fn read_kvs(source: impl Read) -> Result<KVs, WalError> {
    let mut reader = io::BufReader::new(source);
    let mut kvs: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();

    loop {
        // Running into EOF at the top of the loop means we have read all
        // KVs stored in the WAL. An EOF at any other time implies
        // corruption of the WAL.
        if reader.fill_buf()?.is_empty() {
            break;
        }
        let key = encoding::decode_key(&mut reader)?;
        let value = encoding::decode_value(&mut reader)?;

        let checksum = {
            let mut hasher = Hasher::new();
            hasher.update(&(key.len() as encoding::KeyLen).to_le_bytes());
            hasher.update(&key);
            hasher.update(&(value.len() as encoding::ValueLen).to_le_bytes());
            hasher.update(&value);
            hasher.finalize().to_le_bytes()
        };
        let mut stored_checksum = [0; 4];
        reader.read_exact(&mut stored_checksum)?;
        if stored_checksum != checksum {
            return Err(WalError::InvalidChecksum);
        }
        kvs.push((key, value));
    }
    Ok(kvs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutils::seeded_rng;
    use rand::Rng;
    use rand::seq::IndexedRandom;
    use std::mem;
    use tempfile::NamedTempFile;

    const MAX_TEST_KEY_LEN: usize = u16::MAX as usize;
    const MAX_TEST_VAL_LEN: usize = u16::MAX as usize;

    #[test]
    fn wal_roundtrip() {
        let (mut seeded_rng, _) = seeded_rng(None);

        const SYNC_MODES: [SyncMode; 2] = [SyncMode::Sync, SyncMode::Async];
        let mode = *SYNC_MODES
            .choose(&mut seeded_rng)
            .expect("failed to choose sync mode");
        println!("using sync mode {:?}", mode);

        let tmp = NamedTempFile::new().expect("failed to create temp file");
        let mut wal = FileWal::new(tmp.path(), mode).expect("failed to create WAL");

        let mut appended: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        for _ in 0..100 {
            let key_len = seeded_rng.random_range(0..=MAX_TEST_KEY_LEN);
            let val_len = seeded_rng.random_range(0..=MAX_TEST_VAL_LEN);
            let key = generate_rand_bytes(key_len, &mut seeded_rng);
            let value = generate_rand_bytes(val_len, &mut seeded_rng);

            wal.append(&key, &value).expect("failed to append to WAL");
            appended.push((key, value));
        }

        let path = wal.close().expect("failed to close WAL");
        assert_eq!(
            path,
            tmp.path(),
            "expected the path returned on close to match the original path",
        );
        let recovered = FileWal::recover(&path).expect("failed to recover WAL");

        assert_eq!(
            recovered, appended,
            "recovered KVs did not match appended KVs"
        );
        FileWal::delete(&path).expect("failed to delete WAL");
    }

    #[test]
    fn corrupted_key_len() {
        let mut wal = MemWal::new();
        wal.append(b"key", b"value")
            .expect("failed to write KV to WAL");

        wal.bytes[0..mem::size_of::<KeyLen>()].copy_from_slice(&12u16.to_le_bytes());

        MemWal::recover(&wal.bytes)
            .expect_err("expected recovery to fail due to corrupted key length");
    }

    #[test]
    fn corrupted_key_bytes() {
        let mut wal = MemWal::new();
        let key: &[u8] = b"key";
        let value: &[u8] = b"value";
        wal.append(key, value).expect("failed to write KV to WAL");

        let idx_bytes = mem::size_of::<KeyLen>();
        wal.bytes[idx_bytes..idx_bytes + key.len()].copy_from_slice(b"kay");

        MemWal::recover(&wal.bytes)
            .expect_err("expected recovery to fail due to corrupted key bytes");
    }

    #[test]
    fn corrupted_value_len() {
        let mut wal = MemWal::new();
        let key: &[u8] = b"key";
        let value: &[u8] = b"value";
        wal.append(key, value).expect("failed to write KV to WAL");

        let val_idx = mem::size_of::<KeyLen>() + key.len();
        wal.bytes[val_idx..val_idx + mem::size_of::<ValueLen>()]
            .copy_from_slice(&2u32.to_le_bytes());

        MemWal::recover(&wal.bytes)
            .expect_err("expected recovery to fail due to corrupted value length");
    }

    #[test]
    fn corrupted_value_bytes() {
        let mut wal = MemWal::new();
        let key: &[u8] = b"key";
        let value: &[u8] = b"value";
        wal.append(key, value).expect("failed to write KV to WAL");

        let val_idx = mem::size_of::<KeyLen>() + key.len() + mem::size_of::<ValueLen>();
        wal.bytes[val_idx..val_idx + value.len()].copy_from_slice(b"valye");

        MemWal::recover(&wal.bytes)
            .expect_err("expected recovery to fail due to corrupted value bytes");
    }

    #[test]
    fn corrupted_checksum() {
        let mut wal = MemWal::new();
        wal.append(b"key", b"value")
            .expect("failed to write KV to WAL");

        let bytes_len = wal.bytes.len();
        wal.bytes[bytes_len - 1] ^= 0xFF;

        MemWal::recover(&wal.bytes)
            .expect_err("expected recovery to fail due to corrupted checksum");
    }

    fn generate_rand_bytes(n: usize, mut rng: impl Rng) -> Vec<u8> {
        let mut data = vec![0u8; n];
        rng.fill_bytes(&mut data[..]);
        data
    }
}
