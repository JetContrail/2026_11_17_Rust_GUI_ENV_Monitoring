//! AES-256-GCM records for device links that cannot use TLS.
//!
//! ```text
//! record = MAGIC "EMG1" | DIR (1) | SEQ (u64, BE) | ciphertext | tag (16)
//! nonce  = DIR | 0 0 0 | SEQ          AAD = MAGIC | DIR | SEQ
//! ```
//!
//! Each session derives its own key with HKDF-SHA256 from the pre-shared key and random salts
//! chosen by both peers, so a sequence number is never reused under the same key. Records
//! without the magic (plaintext), with a wrong key or tampered bytes, replayed, or reflected
//! back to their sender are rejected.

use std::fmt;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use hkdf::Hkdf;
use sha2::Sha256;

pub const KEY_LEN: usize = 32;
pub const SALT_LEN: usize = 16;
pub const MAGIC: [u8; 4] = *b"EMG1";
pub const HEADER_LEN: usize = 13;
pub const TAG_LEN: usize = 16;
const KDF_INFO: &[u8] = b"envmon aes-256-gcm record v1";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CryptoError {
    #[error("record is not encrypted; plaintext is rejected")]
    Plaintext,
    #[error("record is truncated")]
    Truncated,
    #[error("record authentication failed (wrong key or tampered record)")]
    Authentication,
    #[error("record carries this endpoint's own direction (reflected)")]
    Reflected,
    #[error("replayed or reordered record: seq {seq}, expected at least {expected}")]
    Replay { seq: u64, expected: u64 },
    #[error("sequence numbers exhausted; start a new session")]
    Exhausted,
    #[error("invalid key: {0}")]
    InvalidKey(&'static str),
    #[error("random number generator failed")]
    Rng,
}

/// 256-bit pre-shared key provisioned to a device.
#[derive(Clone, PartialEq, Eq)]
pub struct PresharedKey([u8; KEY_LEN]);

impl fmt::Debug for PresharedKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PresharedKey(..)")
    }
}

impl PresharedKey {
    pub fn new(bytes: [u8; KEY_LEN]) -> Self {
        Self(bytes)
    }

    pub fn generate() -> Result<Self, CryptoError> {
        let mut key = [0; KEY_LEN];
        getrandom::fill(&mut key).map_err(|_| CryptoError::Rng)?;
        Ok(Self(key))
    }

    /// 64 uppercase hexadecimal digits (for provisioning a device).
    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02X}")).collect()
    }

    /// Parses 64 hexadecimal digits.
    pub fn from_hex(s: &str) -> Result<Self, CryptoError> {
        let s = s.trim();
        if s.len() != KEY_LEN * 2 || !s.is_ascii() {
            return Err(CryptoError::InvalidKey("expected 64 hex digits"));
        }
        let mut key = [0; KEY_LEN];
        for (i, byte) in key.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)
                .map_err(|_| CryptoError::InvalidKey("expected 64 hex digits"))?;
        }
        Ok(Self(key))
    }
}

pub fn random_salt() -> Result<[u8; SALT_LEN], CryptoError> {
    let mut salt = [0; SALT_LEN];
    getrandom::fill(&mut salt).map_err(|_| CryptoError::Rng)?;
    Ok(salt)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Host,
    Device,
}

impl Role {
    fn dir(self) -> u8 {
        match self {
            Self::Host => 1,
            Self::Device => 2,
        }
    }

    fn peer(self) -> Self {
        match self {
            Self::Host => Self::Device,
            Self::Device => Self::Host,
        }
    }
}

/// One endpoint of an AES-256-GCM session.
pub struct SecureChannel {
    cipher: Aes256Gcm,
    role: Role,
    send_seq: u64,
    recv_next: u64,
}

impl fmt::Debug for SecureChannel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecureChannel")
            .field("role", &self.role)
            .field("send_seq", &self.send_seq)
            .field("recv_next", &self.recv_next)
            .finish_non_exhaustive()
    }
}

fn header(dir: u8, seq: u64) -> [u8; HEADER_LEN] {
    let mut h = [0; HEADER_LEN];
    h[..4].copy_from_slice(&MAGIC);
    h[4] = dir;
    h[5..].copy_from_slice(&seq.to_be_bytes());
    h
}

fn nonce(dir: u8, seq: u64) -> [u8; 12] {
    let mut n = [0; 12];
    n[0] = dir;
    n[4..].copy_from_slice(&seq.to_be_bytes());
    n
}

impl SecureChannel {
    pub fn new(
        psk: &PresharedKey,
        role: Role,
        host_salt: &[u8; SALT_LEN],
        device_salt: &[u8; SALT_LEN],
    ) -> Result<Self, CryptoError> {
        let mut salt = [0; SALT_LEN * 2];
        salt[..SALT_LEN].copy_from_slice(host_salt);
        salt[SALT_LEN..].copy_from_slice(device_salt);
        let mut key = [0; KEY_LEN];
        Hkdf::<Sha256>::new(Some(&salt), &psk.0)
            .expand(KDF_INFO, &mut key)
            .map_err(|_| CryptoError::InvalidKey("key derivation failed"))?;
        let cipher = Aes256Gcm::new_from_slice(&key)
            .map_err(|_| CryptoError::InvalidKey("bad key length"))?;
        Ok(Self {
            cipher,
            role,
            send_seq: 0,
            recv_next: 0,
        })
    }

    pub fn seal(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let seq = self.send_seq;
        self.send_seq = seq.checked_add(1).ok_or(CryptoError::Exhausted)?;
        let dir = self.role.dir();
        let header = header(dir, seq);
        let ciphertext = self
            .cipher
            .encrypt(
                Nonce::from_slice(&nonce(dir, seq)),
                Payload {
                    msg: plaintext,
                    aad: &header,
                },
            )
            .map_err(|_| CryptoError::Authentication)?;
        let mut out = Vec::with_capacity(HEADER_LEN + ciphertext.len());
        out.extend_from_slice(&header);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    pub fn open(&mut self, record: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if record.get(..MAGIC.len()) != Some(&MAGIC[..]) {
            return Err(CryptoError::Plaintext);
        }
        if record.len() < HEADER_LEN + TAG_LEN {
            return Err(CryptoError::Truncated);
        }
        let dir = record[4];
        if dir == self.role.dir() {
            return Err(CryptoError::Reflected);
        }
        if dir != self.role.peer().dir() {
            return Err(CryptoError::Authentication);
        }
        let mut seq = [0; 8];
        seq.copy_from_slice(&record[5..HEADER_LEN]);
        let seq = u64::from_be_bytes(seq);
        if seq < self.recv_next {
            return Err(CryptoError::Replay {
                seq,
                expected: self.recv_next,
            });
        }
        let plaintext = self
            .cipher
            .decrypt(
                Nonce::from_slice(&nonce(dir, seq)),
                Payload {
                    msg: &record[HEADER_LEN..],
                    aad: &record[..HEADER_LEN],
                },
            )
            .map_err(|_| CryptoError::Authentication)?;
        self.recv_next = seq.saturating_add(1);
        Ok(plaintext)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST_SALT: [u8; SALT_LEN] = [1; SALT_LEN];
    const DEVICE_SALT: [u8; SALT_LEN] = [2; SALT_LEN];
    const MESSAGE: &[u8] = br#"{"type":"door_sensor","door":1,"open":true}"#;

    fn pair(host_key: &PresharedKey, device_key: &PresharedKey) -> (SecureChannel, SecureChannel) {
        (
            SecureChannel::new(host_key, Role::Host, &HOST_SALT, &DEVICE_SALT).unwrap(),
            SecureChannel::new(device_key, Role::Device, &HOST_SALT, &DEVICE_SALT).unwrap(),
        )
    }

    #[test]
    fn valid_records_decrypt_in_both_directions() {
        let key = PresharedKey::generate().unwrap();
        let (mut host, mut device) = pair(&key, &key);
        for _ in 0..3 {
            let record = device.seal(MESSAGE).unwrap();
            assert_eq!(&record[..4], b"EMG1");
            assert!(!record.windows(MESSAGE.len()).any(|w| w == MESSAGE));
            assert_eq!(host.open(&record).unwrap(), MESSAGE);
            assert_eq!(device.open(&host.seal(b"ack").unwrap()).unwrap(), b"ack");
        }
    }

    #[test]
    fn wrong_key_is_rejected() {
        let (mut host, mut device) = pair(
            &PresharedKey::new([0x11; KEY_LEN]),
            &PresharedKey::new([0x22; KEY_LEN]),
        );
        let record = device.seal(MESSAGE).unwrap();
        assert_eq!(host.open(&record), Err(CryptoError::Authentication));
    }

    #[test]
    fn plaintext_is_rejected() {
        let key = PresharedKey::generate().unwrap();
        let (mut host, _) = pair(&key, &key);
        assert_eq!(host.open(MESSAGE), Err(CryptoError::Plaintext));
        assert_eq!(host.open(b""), Err(CryptoError::Plaintext));
        // Magic followed by plaintext is not a valid record either.
        let mut fake = header(2, 0).to_vec();
        fake.extend_from_slice(MESSAGE);
        assert_eq!(host.open(&fake), Err(CryptoError::Authentication));
        assert_eq!(host.open(&header(2, 0)), Err(CryptoError::Truncated));
    }

    #[test]
    fn tampered_record_is_rejected() {
        let key = PresharedKey::generate().unwrap();
        let (mut host, mut device) = pair(&key, &key);
        let record = device.seal(MESSAGE).unwrap();
        for i in [5, HEADER_LEN, record.len() - 1] {
            let mut t = record.clone();
            t[i] ^= 0x01;
            assert_eq!(host.open(&t), Err(CryptoError::Authentication), "byte {i}");
        }
        assert_eq!(host.open(&record).unwrap(), MESSAGE);
    }

    #[test]
    fn replayed_and_reflected_records_are_rejected() {
        let key = PresharedKey::generate().unwrap();
        let (mut host, mut device) = pair(&key, &key);
        let first = device.seal(MESSAGE).unwrap();
        let second = device.seal(MESSAGE).unwrap();
        host.open(&second).unwrap();
        assert_eq!(
            host.open(&first),
            Err(CryptoError::Replay {
                seq: 0,
                expected: 2
            })
        );
        assert_eq!(
            host.open(&second),
            Err(CryptoError::Replay {
                seq: 1,
                expected: 2
            })
        );

        let own = host.seal(MESSAGE).unwrap();
        assert_eq!(host.open(&own), Err(CryptoError::Reflected));
    }

    #[test]
    fn sessions_with_other_salts_cannot_read_each_other() {
        let key = PresharedKey::generate().unwrap();
        let mut device = SecureChannel::new(&key, Role::Device, &HOST_SALT, &DEVICE_SALT).unwrap();
        let mut other = SecureChannel::new(&key, Role::Host, &[9; SALT_LEN], &DEVICE_SALT).unwrap();
        assert_eq!(
            other.open(&device.seal(MESSAGE).unwrap()),
            Err(CryptoError::Authentication)
        );
    }

    #[test]
    fn parses_hex_keys() {
        let hex = "00112233445566778899aabbccddeeff00112233445566778899AABBCCDDEEFF";
        let key = PresharedKey::from_hex(hex).unwrap();
        assert_eq!(key.0[1], 0x11);
        assert_eq!(PresharedKey::from_hex(&key.to_hex()).unwrap(), key);
        assert!(PresharedKey::from_hex(&hex[2..]).is_err());
        assert!(PresharedKey::from_hex(&hex.replace('0', "g")).is_err());
        assert_eq!(format!("{key:?}"), "PresharedKey(..)");
    }
}
