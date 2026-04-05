use crc32fast::Hasher;
use std::{
    fs,
    io::{self, Read, Write},
    mem,
    path::{self, PathBuf},
};

type KeyLen = u16;
type ValueLen = u32;
type KVs = Vec<(Vec<u8>, Vec<u8>)>;

#[derive(thiserror::Error, Debug)]
pub enum WalError {
    #[error("WAL file operation failed")]
    FileOpFailure(#[from] io::Error),
    #[error("Key/value length exceeded, (received {actual:?}, max {max:?})")]
    KVLengthExceeded { actual: usize, max: usize },
    #[error("Invalid checksum detected")]
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
}

impl FileWal {
    /// Creates a new WAL file at the specified path. This will fail if a file
    /// already exists at that path.
    pub fn create(path: impl AsRef<path::Path>, mode: SyncMode) -> Result<Self, WalError> {
        Ok(Self {
            path: path.as_ref().to_path_buf(),
            file: io::BufWriter::new(fs::OpenOptions::new().create_new(true).open(path)?),
            mode,
        })
    }

    /// Opens an existing WAL file at the specified path for appending. This
    /// will fail if no fle exists at the path.
    pub fn open(path: impl AsRef<path::Path>, mode: SyncMode) -> Result<Self, WalError> {
        Ok(Self {
            path: path.as_ref().to_path_buf(),
            file: io::BufWriter::new(fs::OpenOptions::new().append(true).open(path)?),
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
    fn append(&mut self, key: &[u8], value: &[u8]) -> Result<(), WalError> {
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
    pub fn create() -> Self {
        Self { bytes: Vec::new() }
    }

    /// Recovers all key-value pairs from the in-memory WAL in the order in
    /// which they were appended.
    pub fn recover(bytes: &[u8]) -> Result<KVs, WalError> {
        read_kvs(bytes)
    }
}

impl Wal for MemWal {
    fn append(&mut self, key: &[u8], value: &[u8]) -> Result<(), WalError> {
        write_kv(&mut self.bytes, key, value)
    }

    fn sync(&mut self) -> Result<(), WalError> {
        Ok(())
    }
}

/// Writes a key-value pair to the provided WAL sink.
fn write_kv(sink: &mut impl Write, key: &[u8], value: &[u8]) -> Result<(), WalError> {
    if key.len() > KeyLen::MAX as usize {
        return Err(WalError::KVLengthExceeded {
            actual: key.len(),
            max: KeyLen::MAX as usize,
        });
    }
    if value.len() > ValueLen::MAX as usize {
        return Err(WalError::KVLengthExceeded {
            actual: value.len(),
            max: ValueLen::MAX as usize,
        });
    }

    let key_len = (key.len() as KeyLen).to_le_bytes();
    sink.write_all(&key_len)?;
    sink.write_all(key)?;

    let val_len = (value.len() as ValueLen).to_le_bytes();
    sink.write_all(&val_len)?;
    sink.write_all(value)?;

    let checksum = {
        let mut hasher = Hasher::new();
        hasher.update(&key_len);
        hasher.update(key);
        hasher.update(&val_len);
        hasher.update(value);
        hasher.finalize().to_le_bytes()
    };

    sink.write_all(&checksum)?;
    Ok(())
}

/// Reads all key-value pairs from the provided WAL source.
fn read_kvs(source: impl Read) -> Result<KVs, WalError> {
    let mut reader = io::BufReader::new(source);
    let mut kvs: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();

    let mut key_lenb = [0u8; mem::size_of::<KeyLen>()];
    let mut val_lenb = [0u8; mem::size_of::<ValueLen>()];
    loop {
        // Running into EOF at the top of the loop means we have read all
        // KVs stored in the WAL. An EOF at any other time implies
        // corruption of the WAL.
        if let Err(e) = reader.read_exact(&mut key_lenb) {
            if e.kind() == io::ErrorKind::UnexpectedEof {
                break;
            }
            return Err(e.into());
        }
        let key_len = KeyLen::from_le_bytes(key_lenb);
        let mut key = vec![0; key_len as usize];
        reader.read_exact(&mut key)?;

        reader.read_exact(&mut val_lenb)?;
        let val_len = ValueLen::from_le_bytes(val_lenb);
        let mut value = vec![0; val_len as usize];
        reader.read_exact(&mut value)?;

        let checksum = {
            let mut hasher = Hasher::new();
            hasher.update(&key_lenb);
            hasher.update(&key);
            hasher.update(&val_lenb);
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
    use crate::macros::seeded_rng;
    use rand::Rng;
    use rand::seq::IndexedRandom;
    use std::mem;
    use tempfile::NamedTempFile;

    const MAX_TEST_KEY_LEN: usize = u16::MAX as usize;
    const MAX_TEST_VAL_LEN: usize = u16::MAX as usize;

    #[test]
    fn wal_roundtrip() {
        let (mut seeded_rng, _) = seeded_rng!();

        const SYNC_MODES: [SyncMode; 2] = [SyncMode::Sync, SyncMode::Async];
        let mode = *SYNC_MODES
            .choose(&mut seeded_rng)
            .expect("failed to choose sync mode");
        println!("using sync mode {:?}", mode);

        let tmp = NamedTempFile::new().expect("failed to create temp file");
        let mut wal = FileWal::create(tmp.path(), mode).expect("failed to create WAL");

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
        let mut wal = MemWal::create();
        wal.append(b"key", b"value")
            .expect("failed to write KV to WAL");

        wal.bytes[0..mem::size_of::<KeyLen>()].copy_from_slice(&12u16.to_le_bytes());

        MemWal::recover(&wal.bytes)
            .expect_err("expected recovery to fail due to corrupted key length");
    }

    #[test]
    fn corrupted_key_bytes() {
        let mut wal = MemWal::create();
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
        let mut wal = MemWal::create();
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
        let mut wal = MemWal::create();
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
        let mut wal = MemWal::create();
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
