use bittorrent_common::types::InfoHash;
use rand::Rng;
use sha1_smol::Sha1;
use std::{net::IpAddr, time::Duration};
use tokio::time::Instant;

pub struct TokenManager {
    current_secret: [u8; 20],
    previous_secret: [u8; 20],
    last_rotation: Instant,
    rotation_interval: Duration,
}

impl TokenManager {
    pub fn new(rotation_interval: Duration) -> Self {
        let mut current = [0u8; 20];
        let mut previous = [0u8; 20];
        let mut rng = rand::rng();
        rng.fill_bytes(&mut current);
        rng.fill_bytes(&mut previous);

        Self {
            current_secret: current,
            previous_secret: previous,
            last_rotation: Instant::now(),
            rotation_interval,
        }
    }
    /// Generates a token without storing it. Use `IpAddr` to prevent port-spoofing bypasses.
    pub fn generate_token(&self, addr: &IpAddr, info_hash: &InfoHash) -> [u8; 20] {
        Self::hash_token(addr, info_hash, &self.current_secret)
    }
    /// Validates the token against the current and previous secrets.
    pub fn validate_token(&self, addr: &IpAddr, info_hash: &InfoHash, token: [u8; 20]) -> bool {
        let expected_current = Self::hash_token(addr, info_hash, &self.current_secret);
        if token == expected_current {
            return true;
        }

        let expected_previous = Self::hash_token(addr, info_hash, &self.previous_secret);
        token == expected_previous
    }

    /// Rotates secrets if the interval has elapsed. Must be called before generation/validation.
    pub fn check_rotation(&mut self) {
        if self.last_rotation.elapsed() >= self.rotation_interval {
            self.previous_secret = self.current_secret;
            let mut rng = rand::rng();
            rng.fill_bytes(&mut self.current_secret);
            self.last_rotation = Instant::now();
        }
    }

    fn hash_token(addr: &IpAddr, info_hash: &InfoHash, secret: &[u8; 20]) -> [u8; 20] {
        let mut hasher = Sha1::new();
        match addr {
            IpAddr::V4(v4) => hasher.update(&v4.octets()),
            IpAddr::V6(v6) => hasher.update(&v6.octets()),
        }
        hasher.update(info_hash.as_bytes());
        hasher.update(secret);
        hasher.digest().bytes()
    }
}
