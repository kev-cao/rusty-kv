use crate::storage::encoding::Codable;
use crate::storage::kv::{Key, Value};

use crc32fast::Hasher;
use snafu::prelude::*;
use std::{
    fs,
    io::{self, BufRead, Read, Write},
    path::{self, PathBuf},
};

type KVs = Vec<(Key, Value)>;

#[derive(Snafu, Debug)]
pub enum WalError {
    #[snafu(context(false), display("WAL io operation failed: {source}"))]
    Io {
        source: io::Error,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("invalid checksum detected"))]
    InvalidChecksum { backtrace: snafu::Backtrace },
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum SyncMode {
    Sync,
    Async,
}

pub trait Wal {
    /// Appends a key-value pair to the WAL.
    fn append(&mut self, key: &Key, value: &Value) -> Result<(), WalError>;
    /// Synchronously forces the OS to flush all cached data to the physical
    /// disk, ensuring durability. Only meaningful in Async mode.
    fn sync(&mut self) -> Result<(), WalError>;
}

pub struct FileWal {
    path: PathBuf,
    file: io::BufWriter<fs::File>,
    mode: SyncMode,
}

impl FileWal {
    /// Creates a new WAL file at the specified filepath. If the file already
    /// exists, it will be truncated.
    pub fn new(path: impl Into<path::PathBuf>, mode: SyncMode) -> Result<Self, WalError> {
        let path = path.into();
        Ok(Self {
            file: io::BufWriter::new(
                fs::OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .create(true)
                    .open(&path)?,
            ),
            path,
            mode,
        })
    }

    /// Opens an existing WAL file at the specified path for appending. This
    /// will fail if no file exists at the path.
    pub fn open(path: impl Into<path::PathBuf>, mode: SyncMode) -> Result<Self, WalError> {
        let path = path.into();
        Ok(Self {
            file: io::BufWriter::new(fs::OpenOptions::new().append(true).open(&path)?),
            path,
            mode,
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
    fn append(&mut self, key: &Key, value: &Value) -> Result<(), WalError> {
        write_kv(&mut self.file, key, value)?;
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
}

impl MemWal {
    /// Creates a new in-memory WAL instance. This is primarily intended for
    /// testing.
    pub fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    /// Recovers all key-value pairs from the in-memory WAL in the order in
    /// which they were appended.
    pub fn recover(bytes: &[u8]) -> Result<KVs, WalError> {
        read_kvs(bytes)
    }
}

impl Wal for MemWal {
    fn append(&mut self, key: &Key, value: &Value) -> Result<(), WalError> {
        write_kv(&mut self.bytes, key, value)
    }

    fn sync(&mut self) -> Result<(), WalError> {
        Ok(())
    }
}

struct HashingWriter<'a, W: Write> {
    inner: &'a mut W,
    hasher: Hasher,
}

impl<'a, W: Write> Write for HashingWriter<'a, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.hasher.update(buf);
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<'a, W: Write> HashingWriter<'a, W> {
    fn new(writer: &'a mut W, hasher: Hasher) -> Self {
        Self {
            inner: writer,
            hasher,
        }
    }

    fn finalize(self) -> io::Result<()> {
        let checksum = self.hasher.finalize().to_le_bytes();
        self.inner.write_all(&checksum)
    }
}

/// Writes a key-value pair to the provided WAL sink.
fn write_kv(sink: &mut impl Write, key: &Key, value: &Value) -> Result<(), WalError> {
    let mut hw = HashingWriter::new(sink, Hasher::new());
    key.encode(&mut hw)?;
    value.encode(&mut hw)?;
    hw.finalize()?;
    Ok(())
}

/// Reads all key-value pairs from the provided WAL source.
fn read_kvs(source: impl Read) -> Result<KVs, WalError> {
    let mut reader = io::BufReader::new(source);
    let mut kvs: KVs = Vec::new();

    loop {
        // Running into EOF at the top of the loop means we have read all
        // KVs stored in the WAL. An EOF at any other time implies
        // corruption of the WAL.
        if reader.fill_buf()?.is_empty() {
            break;
        }
        let key = Key::decode(&mut reader)?;
        let value = Value::decode(&mut reader)?;

        let checksum = {
            let mut hasher = Hasher::new();
            hasher.update(&key.len().to_le_bytes());
            hasher.update(key.as_ref());
            hasher.update(&value.len().to_le_bytes());
            hasher.update(value.as_ref());
            hasher.finalize().to_le_bytes()
        };
        let mut stored_checksum = [0; 4];
        reader.read_exact(&mut stored_checksum)?;
        if stored_checksum != checksum {
            return InvalidChecksumSnafu.fail();
        }
        kvs.push((key, value));
    }
    Ok(kvs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::kv::{KeyLen, ValueLen};
    use crate::testutils::{generate_rand_bytes, key, seeded_rng, value};
    use rand::Rng;
    use rand::seq::IndexedRandom;
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

        let mut appended: KVs = Vec::new();
        for _ in 0..100 {
            let key_len = seeded_rng.random_range(0..=MAX_TEST_KEY_LEN);
            let val_len = seeded_rng.random_range(0..=MAX_TEST_VAL_LEN);
            let key = key(&generate_rand_bytes(key_len, &mut seeded_rng));
            let value = value(&generate_rand_bytes(val_len, &mut seeded_rng));

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
        wal.append(&key(b"key".as_ref()), &value(b"value".as_ref()))
            .expect("failed to write KV to WAL");

        wal.bytes[0..size_of::<KeyLen>()].copy_from_slice(&12u16.to_le_bytes());

        MemWal::recover(&wal.bytes)
            .expect_err("expected recovery to fail due to corrupted key length");
    }

    #[test]
    fn corrupted_key_bytes() {
        let mut wal = MemWal::new();
        let key: &Key = &key(b"key".as_ref());
        let value: &Value = &value(b"value".as_ref());
        wal.append(key, value).expect("failed to write KV to WAL");

        let idx_bytes = size_of::<KeyLen>();
        wal.bytes[idx_bytes..idx_bytes + key.len() as usize].copy_from_slice(b"kay");

        MemWal::recover(&wal.bytes)
            .expect_err("expected recovery to fail due to corrupted key bytes");
    }

    #[test]
    fn corrupted_value_len() {
        let mut wal = MemWal::new();
        let key: &Key = &key(b"key".as_ref());
        let value: &Value = &value(b"value".as_ref());
        wal.append(key, value).expect("failed to write KV to WAL");

        let val_idx = size_of::<KeyLen>() + key.len() as usize;
        wal.bytes[val_idx..val_idx + size_of::<ValueLen>()].copy_from_slice(&2u32.to_le_bytes());

        MemWal::recover(&wal.bytes)
            .expect_err("expected recovery to fail due to corrupted value length");
    }

    #[test]
    fn corrupted_value_bytes() {
        let mut wal = MemWal::new();
        let key: &Key = &key(b"key".as_ref());
        let value: &Value = &value(b"value".as_ref());
        wal.append(key, value).expect("failed to write KV to WAL");

        let val_idx = size_of::<KeyLen>() + key.len() as usize + size_of::<ValueLen>();
        wal.bytes[val_idx..val_idx + value.len() as usize].copy_from_slice(b"valye");

        MemWal::recover(&wal.bytes)
            .expect_err("expected recovery to fail due to corrupted value bytes");
    }

    #[test]
    fn corrupted_checksum() {
        let mut wal = MemWal::new();
        let key: &Key = &key(b"key".as_ref());
        let value: &Value = &value(b"value".as_ref());
        wal.append(key, value).expect("failed to write KV to WAL");

        let bytes_len = wal.bytes.len();
        wal.bytes[bytes_len - 1] ^= 0xFF;

        MemWal::recover(&wal.bytes)
            .expect_err("expected recovery to fail due to corrupted checksum");
    }
}
