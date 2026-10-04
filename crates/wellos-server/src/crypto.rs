//! Application-level encryption for retained location data.
//!
//! Exact addresses and live transport coordinates that must be kept
//! operationally are sealed with AES-256-GCM under a rotation-ready keyring:
//! every ciphertext records the key id it was sealed with, new writes use the
//! active key, and any configured key can still open older rows. Keys arrive
//! only through configuration (`WELLOS_LOCATION_ENCRYPTION_KEYS`), never from
//! the database, and are never logged.

use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{AeadCore, Aes256Gcm, Key, Nonce};
use base64::Engine;
use std::collections::BTreeMap;

const FORMAT_VERSION: u8 = 1;
const NONCE_LEN: usize = 12;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CryptoError {
    #[error("ciphertext is malformed")]
    Malformed,
    #[error("ciphertext was sealed with an unknown key id")]
    UnknownKey,
    #[error("ciphertext failed authentication")]
    Authentication,
}

/// A set of named AES-256 keys with exactly one active for new writes.
#[derive(Clone)]
pub struct Keyring {
    keys: BTreeMap<String, Key<Aes256Gcm>>,
    active: String,
}

impl std::fmt::Debug for Keyring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Keyring")
            .field("key_ids", &self.keys.keys().collect::<Vec<_>>())
            .field("active", &self.active)
            .finish()
    }
}

impl Keyring {
    /// Parse `kid:base64(32 bytes)[,kid:base64(32 bytes)...]` plus the id of
    /// the key new writes use. Every rule fails closed: short keys,
    /// duplicate ids, an unknown active id or an empty ring abort startup.
    pub fn parse(keys: &str, active: &str) -> anyhow::Result<Self> {
        let mut parsed = BTreeMap::new();
        for entry in keys.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            let Some((kid, material)) = entry.split_once(':') else {
                anyhow::bail!("encryption key entries must be `<key-id>:<base64 key>`");
            };
            let kid = kid.trim();
            if kid.is_empty()
                || kid.len() > 64
                || !kid
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
            {
                anyhow::bail!("encryption key ids must be 1-64 characters of [A-Za-z0-9._-]");
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(material.trim())
                .map_err(|_| anyhow::anyhow!("encryption key '{kid}' is not valid base64"))?;
            if bytes.len() != 32 {
                anyhow::bail!("encryption key '{kid}' must decode to exactly 32 bytes");
            }
            if parsed
                .insert(kid.to_string(), *Key::<Aes256Gcm>::from_slice(&bytes))
                .is_some()
            {
                anyhow::bail!("encryption key id '{kid}' is listed twice");
            }
        }
        if parsed.is_empty() {
            anyhow::bail!("at least one encryption key is required");
        }
        let active = active.trim();
        if !parsed.contains_key(active) {
            anyhow::bail!("the active encryption key id '{active}' is not in the keyring");
        }
        Ok(Self {
            keys: parsed,
            active: active.to_string(),
        })
    }

    pub fn active_key_id(&self) -> &str {
        &self.active
    }

    pub fn key_ids(&self) -> impl Iterator<Item = &str> {
        self.keys.keys().map(String::as_str)
    }

    /// Seal `plaintext` with the active key. Layout:
    /// `version(1) | kid_len(1) | kid | nonce(12) | ciphertext+tag`, with the
    /// version and key id authenticated as associated data.
    pub fn seal(&self, plaintext: &[u8]) -> Vec<u8> {
        let key = self
            .keys
            .get(&self.active)
            .expect("active key is validated at construction");
        let cipher = Aes256Gcm::new(key);
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let kid = self.active.as_bytes();
        let mut aad = vec![FORMAT_VERSION, kid.len() as u8];
        aad.extend_from_slice(kid);
        let sealed = cipher
            .encrypt(
                &nonce,
                aes_gcm::aead::Payload {
                    msg: plaintext,
                    aad: &aad,
                },
            )
            .expect("AES-GCM encryption of in-memory data cannot fail");
        let mut out = aad;
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&sealed);
        out
    }

    /// Open a ciphertext produced by [`Keyring::seal`] under any key still in
    /// the ring.
    pub fn open(&self, sealed: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if sealed.len() < 2 || sealed[0] != FORMAT_VERSION {
            return Err(CryptoError::Malformed);
        }
        let kid_len = sealed[1] as usize;
        let header_len = 2 + kid_len;
        if sealed.len() < header_len + NONCE_LEN + 16 {
            return Err(CryptoError::Malformed);
        }
        let kid =
            std::str::from_utf8(&sealed[2..header_len]).map_err(|_| CryptoError::Malformed)?;
        let key = self.keys.get(kid).ok_or(CryptoError::UnknownKey)?;
        let aad = &sealed[..header_len];
        let nonce = Nonce::from_slice(&sealed[header_len..header_len + NONCE_LEN]);
        let body = &sealed[header_len + NONCE_LEN..];
        Aes256Gcm::new(key)
            .decrypt(nonce, aes_gcm::aead::Payload { msg: body, aad })
            .map_err(|_| CryptoError::Authentication)
    }

    /// Which key sealed a ciphertext, for rotation reporting.
    pub fn key_id_of(sealed: &[u8]) -> Option<&str> {
        if sealed.len() < 2 || sealed[0] != FORMAT_VERSION {
            return None;
        }
        let kid_len = sealed[1] as usize;
        sealed
            .get(2..2 + kid_len)
            .and_then(|b| std::str::from_utf8(b).ok())
    }

    /// Deterministic test/development keyring built from a label. Only
    /// compiled with fixtures so a production binary cannot hold a
    /// predictable key.
    #[cfg(feature = "dev-fixtures")]
    pub fn synthetic(label: &str) -> Self {
        use sha2::Digest;
        let digest = sha2::Sha256::digest(format!("wellos-synthetic-key:{label}").as_bytes());
        let encoded = base64::engine::general_purpose::STANDARD.encode(digest);
        Self::parse(&format!("synthetic-1:{encoded}"), "synthetic-1")
            .expect("synthetic keyring is well-formed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> String {
        base64::engine::general_purpose::STANDARD.encode([byte; 32])
    }

    #[test]
    fn seal_and_open_roundtrip_with_key_id() {
        let ring = Keyring::parse(&format!("k1:{}", key(1)), "k1").unwrap();
        let sealed = ring.seal(b"Carrer de la Mar 12, 07800");
        assert_eq!(Keyring::key_id_of(&sealed), Some("k1"));
        assert_eq!(ring.open(&sealed).unwrap(), b"Carrer de la Mar 12, 07800");
        assert_ne!(
            ring.seal(b"same"),
            ring.seal(b"same"),
            "fresh nonce per seal"
        );
    }

    #[test]
    fn rotation_keeps_old_ciphertexts_readable_and_writes_with_active() {
        let old = Keyring::parse(&format!("k1:{}", key(1)), "k1").unwrap();
        let sealed_old = old.seal(b"origin");
        let rotated = Keyring::parse(&format!("k1:{},k2:{}", key(1), key(2)), "k2").unwrap();
        assert_eq!(rotated.open(&sealed_old).unwrap(), b"origin");
        assert_eq!(Keyring::key_id_of(&rotated.seal(b"x")), Some("k2"));
        let retired = Keyring::parse(&format!("k2:{}", key(2)), "k2").unwrap();
        assert_eq!(retired.open(&sealed_old), Err(CryptoError::UnknownKey));
    }

    #[test]
    fn tampering_is_detected() {
        let ring = Keyring::parse(&format!("k1:{}", key(1)), "k1").unwrap();
        let mut sealed = ring.seal(b"secret");
        let last = sealed.len() - 1;
        sealed[last] ^= 0x01;
        assert_eq!(ring.open(&sealed), Err(CryptoError::Authentication));
        assert_eq!(ring.open(b"\x01"), Err(CryptoError::Malformed));
        assert_eq!(ring.open(b"\x02abc"), Err(CryptoError::Malformed));
    }

    #[test]
    fn parsing_fails_closed() {
        assert!(Keyring::parse("", "k1").is_err());
        assert!(Keyring::parse("k1:short", "k1").is_err());
        assert!(Keyring::parse(&format!("k1:{}", key(1)), "k2").is_err());
        assert!(Keyring::parse(&format!("k1:{},k1:{}", key(1), key(2)), "k1").is_err());
        assert!(Keyring::parse(&format!("bad id:{}", key(1)), "bad id").is_err());
        let ring = Keyring::parse(&format!("k1:{}", key(1)), "k1").unwrap();
        assert!(!format!("{ring:?}").contains(&key(1)));
    }
}
