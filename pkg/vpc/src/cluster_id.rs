use rand::Rng;

/// Generate a random nonzero cluster ID.
pub fn generate_cluster_id() -> u32 {
    let mut rng = rand::rng();
    loop {
        let id: u32 = rng.random();
        if id != 0 {
            return id;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_nonzero() {
        for _ in 0..100 {
            assert_ne!(generate_cluster_id(), 0);
        }
    }
}
