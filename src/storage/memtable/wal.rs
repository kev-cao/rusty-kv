use crc32fast::Hasher;
use std::{
    fs,
    io::{self, Read, Write},
    mem,
    path::{self, PathBuf},
};

type KeyLen = u16;
type ValueLen = u32;

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
    /// Opens an existing WAL file or creates a new one if it doesn't exist.
    fn open(path: impl AsRef<path::Path>, mode: SyncMode) -> Result<Self, WalError>
    where
        Self: Sized;
    /// Recovers all key-value pairs from the WAL file in the order in which
    /// they were appended.
    fn recover(path: impl AsRef<path::Path>) -> Result<Vec<(Vec<u8>, Vec<u8>)>, WalError>;
    /// Deletes the WAL file from disk. This is typically called after a
    /// successful flush of the memtable to disk, as the WAL is no longer needed
    /// once its contents have been safely persisted.
    fn delete(path: impl AsRef<path::Path>) -> Result<(), WalError>;

    /// Appends a key-value pair to the WAL.
    fn append(&mut self, key: &[u8], value: &[u8]) -> Result<(), WalError>;
    /// Synchronously forces the OS to flush all cached data to the physical
    /// disk, ensuring durability. Only meaningful in Async mode.
    fn sync(&mut self) -> Result<(), WalError>;
    /// Closes the current WAL and returns its path for later deletion. After
    /// calling this, the WAL can no longer accept writes.
    fn close(self) -> Result<PathBuf, WalError>
    where
        Self: Sized;
}

pub struct FileWal {
    path: PathBuf,
    file: io::BufWriter<fs::File>,
    mode: SyncMode,
}

impl Wal for FileWal {
    fn open(path: impl AsRef<path::Path>, mode: SyncMode) -> Result<Self, WalError> {
        Ok(Self {
            path: path.as_ref().to_path_buf(),
            file: io::BufWriter::new(
                fs::OpenOptions::new()
                    .append(true)
                    .create(true)
                    .open(path)?,
            ),
            mode,
        })
    }

    fn recover(path: impl AsRef<path::Path>) -> Result<Vec<(Vec<u8>, Vec<u8>)>, WalError> {
        let file = fs::File::open(path)?;
        let mut reader = io::BufReader::new(file);
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

    fn delete(path: impl AsRef<path::Path>) -> Result<(), WalError> {
        fs::remove_file(path)?;
        Ok(())
    }

    fn append(&mut self, key: &[u8], value: &[u8]) -> Result<(), WalError> {
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
        self.file.write_all(&key_len)?;
        self.file.write_all(key)?;

        let val_len = (value.len() as ValueLen).to_le_bytes();
        self.file.write_all(&val_len)?;
        self.file.write_all(value)?;

        let checksum = {
            let mut hasher = Hasher::new();
            hasher.update(&key_len);
            hasher.update(key);
            hasher.update(&val_len);
            hasher.update(value);
            hasher.finalize().to_le_bytes()
        };

        self.file.write_all(&checksum)?;
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

    fn close(mut self) -> Result<PathBuf, WalError> {
        self.sync()?;
        Ok(self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};
    use rand::{rng, seq::IndexedRandom};
    use tempfile::NamedTempFile;

    #[test]
    fn wal_roundtrip() {
        let seed: u64 = rng().random();
        println!("using seed {}", seed);
        let mut seeded_rng = StdRng::seed_from_u64(seed);

        const SYNC_MODES: [SyncMode; 2] = [SyncMode::Sync, SyncMode::Async];
        let mode = *SYNC_MODES
            .choose(&mut seeded_rng)
            .expect("failed to choose sync mode");
        println!("using sync mode {:?}", mode);

        let tmp = NamedTempFile::new().expect("failed to create temp file");
        let mut wal = FileWal::open(tmp.path(), mode).expect("failed to open WAL");

        const MAX_TEST_KEY_LEN: usize = u16::MAX as usize;
        const MAX_TEST_VAL_LEN: usize = u16::MAX as usize;

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

    fn generate_rand_bytes(n: usize, mut rng: impl Rng) -> Vec<u8> {
        let mut data = vec![0u8; n];
        rng.fill_bytes(&mut data[..]);
        data
    }
}
