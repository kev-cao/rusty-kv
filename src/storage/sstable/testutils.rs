use crate::testutils::{TestResult, generate_rand_bytes, key, value};
use rand::Rng;
use tempfile::{TempDir, tempdir};

use super::*;

const MAX_TEST_KEY_LEN: usize = u16::MAX as usize;
const MAX_TEST_VAL_LEN: usize = u16::MAX as usize;

pub struct SstFixture {
    // We return the temp dir so that it is not deleted until the test is
    // done.
    pub dir: TempDir,
    pub filepath: PathBuf,
    pub sstable: SsTable,
    /// Contains the entries that were added to the SSTable, in sorted order.
    pub entries: Vec<(Key, Value)>,
}

impl SstFixture {
    /// Fetches the expected value for a given key from the entries that were
    /// added to the SSTable.
    pub fn get_value(&self, key: &Key) -> Option<&Value> {
        self.entries
            .binary_search_by(|(k, _)| k.cmp(key))
            .ok()
            .map(|idx| &self.entries[idx].1)
    }
}

pub fn create_random_sstable(num_entries: u32, mut rng: impl Rng) -> TestResult<SstFixture> {
    let dir = tempdir().expect("create tmp dir");
    let tmp_sst = dir.path().join("tempdata.sst");
    let opts = SstBuildOpts::default();
    let mut builder = SstBuilder::new(&tmp_sst, opts)?;

    let mut entries = Vec::new();
    // Inserting data into an SSTable must always be in sorted order, so we
    // generate random keys but prefix them with a monotonically increasing
    // number to ensure they are sorted.
    for (prefix, _) in (0..num_entries).enumerate() {
        let key_len = rng.random_range(0..=MAX_TEST_KEY_LEN);
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
    Ok(SstFixture {
        dir,
        filepath: tmp_sst,
        sstable,
        entries,
    })
}

pub fn create_sstable_with_kvs(
    mut kvs: Vec<(Key, Value)>,
    opts: SstBuildOpts,
) -> TestResult<SstFixture> {
    let dir = tempdir().expect("create tmp dir");
    let tmp_sst = dir.path().join("tempdata.sst");
    let mut builder = SstBuilder::new(&tmp_sst, opts)?;

    // SSTable requires keys to be added in sorted order, so we sort.
    kvs.sort_by(|(key1, _), (key2, _)| key1.cmp(key2));

    for (key, value) in &kvs {
        builder.add(key, value)?;
    }
    builder.finish()?;

    let sstable = SsTable::open(&tmp_sst)?;
    Ok(SstFixture {
        dir,
        filepath: tmp_sst,
        sstable,
        entries: kvs,
    })
}
