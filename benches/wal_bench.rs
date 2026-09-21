use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use rusty_kv::{FileWal, Key, SyncMode, Value, Wal};
use std::hint::black_box;
use tempfile::NamedTempFile;

const MAX_TEST_KEY_LEN: usize = u8::MAX as usize;
const MAX_TEST_VAL_LEN: usize = u8::MAX as usize;

fn wal_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("wal_append");

    for mode in [SyncMode::Async, SyncMode::Sync] {
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{:?}", mode)),
            &mode,
            |b, &mode| {
                // Writing to /tmp does not provide accurate results, as /tmp is
                // often a tmpfs (in-memory) filesystem. Instead, we create a
                // temporary file in the current directory, which is likely on
                // disk, to get more realistic performance measurements.
                let tmp = NamedTempFile::new_in(".").expect("failed to create temp file");
                let mut wal = FileWal::open(tmp.path(), mode).expect("failed to create WAL");

                // Simple constant data
                let key = Key::try_from(vec![0u8; MAX_TEST_KEY_LEN])?;
                let value = Value::try_from(vec![b'x'; MAX_TEST_VAL_LEN])?;

                b.iter(|| {
                    wal.append(black_box(&key), black_box(&value))
                        .expect("append failed")
                });

                wal.close().expect("failed to close WAL");
            },
        );
    }

    group.finish();
}

criterion_group!(benches, wal_benchmark);
criterion_main!(benches);
