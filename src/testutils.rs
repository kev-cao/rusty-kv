use rand::{Rng, SeedableRng, rngs::StdRng};

/// Returns a seeded random number generator and the seed used. If no seed is
/// provided, a random seed is generated.
pub fn seeded_rng(seed: Option<u64>) -> (StdRng, u64) {
    let seed = seed.unwrap_or_else(|| Rng::random(&mut rand::rng()));
    println!("using seed {}", seed);
    (<StdRng as SeedableRng>::seed_from_u64(seed), seed)
}
