use crate::storage::kv::{Key, Value};
use rand::{Rng, SeedableRng, rngs::StdRng};

pub(crate) trait ReportableError: std::error::Error {
    fn backtrace(&self) -> Option<&snafu::Backtrace> {
        None
    }
}

impl<T: std::error::Error + snafu::ErrorCompat> ReportableError for T {
    fn backtrace(&self) -> Option<&snafu::Backtrace> {
        snafu::ErrorCompat::backtrace(self)
    }
}

pub struct Trace(pub Box<dyn ReportableError>);

impl std::fmt::Debug for Trace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "{}", self.0)?;
        let mut cause = std::error::Error::source(&*self.0);
        if cause.is_some() {
            writeln!(f, "\nCaused by these errors (recent errors listed first):")?;
            let mut i = 1;
            while let Some(e) = cause {
                writeln!(f, "  {}: {}", i, e)?;
                cause = e.source();
                i += 1;
            }
        }
        if let Some(bt) = self.0.backtrace() {
            writeln!(f, "\nBacktrace:\n{:?}", bt)?;
        }
        Ok(())
    }
}

impl<T: ReportableError + 'static> From<T> for Trace {
    fn from(e: T) -> Self {
        Trace(Box::new(e))
    }
}

/// All unit tests should return a TestResult, which gives the test error output
/// a backtrace when the test fails.
pub type TestResult<T> = Result<T, Trace>;

/// Returns a seeded random number generator and the seed used. If no seed is
/// provided, a random seed is generated.
pub fn seeded_rng(seed: Option<u64>) -> (StdRng, u64) {
    let seed = seed.unwrap_or_else(|| Rng::random(&mut rand::rng()));
    println!("using seed {}", seed);
    (<StdRng as SeedableRng>::seed_from_u64(seed), seed)
}

/// Generates a vector of `n` random bytes.
pub fn generate_rand_bytes(n: usize, mut rng: impl Rng) -> Vec<u8> {
    let mut data = vec![0u8; n];
    rng.fill_bytes(&mut data[..]);
    data
}

/// Convenience helper for converting byte slice to a Key.
pub fn key(bytes: &[u8]) -> Key {
    Key::try_from(bytes).expect("expected safe bytes when casting to key")
}

/// Convenience helper for converting byte slice to a Value.
pub fn value(bytes: &[u8]) -> Value {
    Value::try_from(bytes).expect("expected safe bytes when casting to value")
}
