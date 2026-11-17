//! Bearer-token authentication and role-based access control.
//!
//! Only SHA-256 hashes of tokens are stored; lookups compare hashes in constant time.

use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// Roles in increasing order of privilege; each role includes the ones below it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Read users, doors, alarms, history and sensor data.
    Viewer,
    /// Viewer + remote unlock and alarm acknowledgement.
    Operator,
    /// Operator + user management.
    Admin,
}

/// One API client as stored in the token file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenEntry {
    pub name: String,
    pub role: Role,
    /// Lowercase hex SHA-256 of the bearer token.
    pub token_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Principal {
    pub name: String,
    pub role: Role,
}

#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    #[error("token file: {0}")]
    Io(#[from] std::io::Error),
    #[error("token file: {0}")]
    Json(#[from] serde_json::Error),
    #[error("token entry {0:?}: token_sha256 must be 64 hex digits")]
    BadHash(String),
    #[error("random source: {0}")]
    Random(String),
}

#[derive(Debug, Default, Clone)]
pub struct TokenStore {
    entries: Vec<(Principal, [u8; 32])>,
}

pub fn hash_token(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

pub fn hash_token_hex(token: &str) -> String {
    hash_token(token)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A new random 256-bit token (hex).
pub fn generate_token() -> Result<String, TokenError> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| TokenError::Random(e.to_string()))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn parse_hash(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 || !hex.is_ascii() {
        return None;
    }
    let mut out = [0_u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

impl TokenStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a client with a plaintext token (kept only as a hash).
    pub fn add(&mut self, name: impl Into<String>, role: Role, token: &str) {
        let principal = Principal {
            name: name.into(),
            role,
        };
        self.entries.push((principal, hash_token(token)));
    }

    pub fn add_entry(&mut self, entry: &TokenEntry) -> Result<(), TokenError> {
        let hash = parse_hash(&entry.token_sha256)
            .ok_or_else(|| TokenError::BadHash(entry.name.clone()))?;
        let principal = Principal {
            name: entry.name.clone(),
            role: entry.role,
        };
        self.entries.push((principal, hash));
        Ok(())
    }

    /// Loads a JSON array of [`TokenEntry`].
    pub fn load(path: impl AsRef<Path>) -> Result<Self, TokenError> {
        let entries: Vec<TokenEntry> = serde_json::from_str(&std::fs::read_to_string(path)?)?;
        let mut store = Self::new();
        for e in &entries {
            store.add_entry(e)?;
        }
        Ok(store)
    }

    pub fn authenticate(&self, token: &str) -> Option<Principal> {
        let hash = hash_token(token);
        let mut found = None;
        for (principal, stored) in &self.entries {
            if bool::from(stored.ct_eq(&hash)) && found.is_none() {
                found = Some(principal.clone());
            }
        }
        found
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn roles_are_ordered_by_privilege() {
        assert!(Role::Viewer < Role::Operator && Role::Operator < Role::Admin);
    }

    #[test]
    fn authenticates_only_known_tokens() {
        let mut store = TokenStore::new();
        store.add("ops", Role::Operator, "secret-1");
        store
            .add_entry(&TokenEntry {
                name: "cloud".into(),
                role: Role::Viewer,
                token_sha256: hash_token_hex("secret-2"),
            })
            .unwrap();
        assert_eq!(store.authenticate("secret-1").unwrap().role, Role::Operator);
        assert_eq!(store.authenticate("secret-2").unwrap().name, "cloud");
        assert!(store.authenticate("secret-3").is_none());
        assert!(store.authenticate("").is_none());
    }

    #[test]
    fn rejects_malformed_hashes() {
        let bad = TokenEntry {
            name: "x".into(),
            role: Role::Admin,
            token_sha256: "abc".into(),
        };
        assert!(TokenStore::new().add_entry(&bad).is_err());
    }

    #[test]
    fn generated_tokens_are_random() {
        let a = generate_token().unwrap();
        assert_eq!(a.len(), 64);
        assert_ne!(a, generate_token().unwrap());
    }
}
