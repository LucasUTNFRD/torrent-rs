//! Token management for announce_peer validation (BEP 5).
//!
//! Per BEP 5, tokens are used to prevent malicious hosts from signing up other
//! hosts for torrents. The token is generated from the querying node's IP address
//! and a secret that rotates periodically.
//!
//! Token format: Hash of (IP || secret)
//! - Secret rotates every 5 minutes
//! - Tokens up to 10 minutes old are accepted (current + previous secret)

use std::{
    hash::{Hash, Hasher},
    net::Ipv4Addr,
    time::{Duration, Instant},
};

use rand::Rng;

/// Token rotation interval (5 minutes per BEP 5).
const ROTATION_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Secret size in bytes.
const SECRET_SIZE: usize = 20;

/// Token size in bytes (truncated hash).
const TOKEN_SIZE: usize = 8;

/// Manages token generation and validation for announce_peer requests.
///
/// Tokens are bound to the querying node's IP address and a rotating secret.
/// This prevents nodes from announcing on behalf of other IPs.
pub struct TokenManager {
    /// Current secret for token generation.
    current_secret: [u8; SECRET_SIZE],
    /// Previous secret (for validating tokens up to 10 min old).
    previous_secret: [u8; SECRET_SIZE],
    /// When the current secret was created.
    last_rotation: Instant,
}

impl TokenManager {
    /// Create a new token manager with random secrets.
    pub fn new() -> Self {
        let mut current = [0u8; SECRET_SIZE];
        let mut previous = [0u8; SECRET_SIZE];
        rand::rng().fill_bytes(&mut current);
        rand::rng().fill_bytes(&mut previous);

        Self {
            current_secret: current,
            previous_secret: previous,
            last_rotation: Instant::now(),
        }
    }

    /// Generate a token for the given IPv4 address.
    ///
    /// The token is valid for up to 10 minutes (until the secret rotates twice).
    pub fn generate(&mut self, ip: &Ipv4Addr) -> Vec<u8> {
        self.maybe_rotate();
        Self::compute_token(ip, &self.current_secret)
    }

    /// Validate a token against current or previous secret.
    ///
    /// Returns true if the token is valid for the given IP address.
    pub fn validate(&mut self, ip: &Ipv4Addr, token: &[u8]) -> bool {
        self.maybe_rotate();

        let current_token = Self::compute_token(ip, &self.current_secret);
        let previous_token = Self::compute_token(ip, &self.previous_secret);

        token == current_token.as_slice() || token == previous_token.as_slice()
    }

    /// Rotate the secret if the rotation interval has passed.
    fn maybe_rotate(&mut self) {
        if self.last_rotation.elapsed() >= ROTATION_INTERVAL {
            self.previous_secret = self.current_secret;
            rand::rng().fill_bytes(&mut self.current_secret);
            self.last_rotation = Instant::now();
            tracing::debug!("Token secret rotated");
        }
    }

    /// Compute a token from IP and secret.
    ///
    /// Uses a fast hash function. For strict BEP 5 compliance, SHA1 could be used,
    /// but this is sufficient for the security guarantees needed.
    fn compute_token(ip: &Ipv4Addr, secret: &[u8; SECRET_SIZE]) -> Vec<u8> {
        use std::collections::hash_map::DefaultHasher;

        let mut hasher = DefaultHasher::new();
        ip.octets().hash(&mut hasher);
        secret.hash(&mut hasher);
        let hash = hasher.finish();

        // Return first TOKEN_SIZE bytes
        hash.to_be_bytes()[..TOKEN_SIZE].to_vec()
    }
}

impl Default for TokenManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_returns_consistent_token_for_same_ip() {
        let mut manager = TokenManager::new();
        let ip = Ipv4Addr::new(192, 168, 1, 1);

        let token1 = manager.generate(&ip);
        let token2 = manager.generate(&ip);

        assert_eq!(token1, token2);
        assert_eq!(token1.len(), TOKEN_SIZE);
    }

    #[test]
    fn different_ips_get_different_tokens() {
        let mut manager = TokenManager::new();
        let ip1 = Ipv4Addr::new(192, 168, 1, 1);
        let ip2 = Ipv4Addr::new(192, 168, 1, 2);

        let token1 = manager.generate(&ip1);
        let token2 = manager.generate(&ip2);

        assert_ne!(token1, token2);
    }

    #[test]
    fn validate_accepts_valid_token() {
        let mut manager = TokenManager::new();
        let ip = Ipv4Addr::new(10, 0, 0, 1);

        let token = manager.generate(&ip);
        assert!(manager.validate(&ip, &token));
    }

    #[test]
    fn validate_rejects_invalid_token() {
        let mut manager = TokenManager::new();
        let ip = Ipv4Addr::new(10, 0, 0, 1);

        let fake_token = vec![0u8; TOKEN_SIZE];
        assert!(!manager.validate(&ip, &fake_token));
    }

    #[test]
    fn validate_rejects_token_for_wrong_ip() {
        let mut manager = TokenManager::new();
        let ip1 = Ipv4Addr::new(192, 168, 1, 1);
        let ip2 = Ipv4Addr::new(192, 168, 1, 2);

        let token = manager.generate(&ip1);
        assert!(!manager.validate(&ip2, &token));
    }
}
