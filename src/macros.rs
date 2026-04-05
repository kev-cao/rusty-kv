macro_rules! seeded_rng {
    () => {{
        let seed = rand::Rng::random(&mut rand::rng());
        println!("using seed {}", seed);
        let rng = <rand::rngs::StdRng as rand::SeedableRng>::seed_from_u64(seed);
        (rng, seed)
    }};
    ($seed:expr) => {{
        println!("using seed {}", $seed);
        let rng = <rand::rngs::StdRng as rand::SeedableRng>::seed_from_u64($seed);
        (rng, $seed)
    }};
}

pub(crate) use seeded_rng;
