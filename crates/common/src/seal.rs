//! Sealing with ChaCha20-Poly1305 (KeyVault scheme, spec §3).
//!
//! Every stored value carries a prefix. [`Sealer::seal`] produces
//! `v1:<base64 nonce12 || ciphertext>` with a fresh random nonce under the
//! key SHA-256(`"setu-seal-v1"` || secret key). [`Sealer::seal_plain`]
//! produces `plain:<value>` for values that are deliberately not secret.
//! [`Sealer::open`] decrypts `v1:`, strips `plain:`, and refuses anything
//! else with [`SealError::Unrecognised`], so an unprefixed value can never
//! be mistaken for either.

// chacha20poly1305 0.10 pulls generic-array 0.14, whose build script marks the
// crate deprecated to nudge to 1.x. We stay on 0.10 per the MVP plan, so allow
// the deprecation signal locally. Revisit when upgrading chacha20poly1305 past
// 0.10; do not add new uses of deprecated APIs under this allow.
#![allow(deprecated)]

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    ChaCha20Poly1305, Key, Nonce,
};
use rand::TryRngCore;
use sha2::{Digest, Sha256};

use crate::config::SecretKey;

/// Key-derivation label (domain separation from other uses of the key).
const SEAL_LABEL: &[u8] = b"setu-seal-v1";

/// Encrypts and decrypts secret values.
#[derive(Clone)]
pub struct Sealer {
    cipher: ChaCha20Poly1305,
}

impl std::fmt::Debug for Sealer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Sealer([redacted])")
    }
}

/// Errors from [`Sealer::open`].
#[derive(Debug, thiserror::Error)]
pub enum SealError {
    #[error("invalid sealed value format")]
    InvalidFormat,
    #[error("invalid sealed encoding")]
    InvalidEncoding,
    #[error("decryption failed")]
    DecryptionFailed,
    #[error("decrypted value is not valid UTF-8")]
    InvalidUtf8,
    #[error("stored value has no recognised prefix")]
    Unrecognised,
}

impl Sealer {
    pub fn new(secret: &SecretKey) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(SEAL_LABEL);
        hasher.update(secret.bytes());
        let derived = hasher.finalize();
        Self {
            cipher: ChaCha20Poly1305::new(Key::from_slice(&derived)),
        }
    }

    /// Seal `plain` as `v1:<base64 nonce || ciphertext>`.
    ///
    /// Infallible by design (the plan fixes this signature): the nonce comes
    /// from the OS RNG and encryption with a valid key and fresh nonce cannot
    /// fail, so the two `expect`s below fire only on OS-level entropy failure.
    #[allow(clippy::expect_used)] // reviewed: kept as-is (R150)
    pub fn seal(&self, plain: &str) -> String {
        let mut nonce_bytes = [0u8; 12];
        rand::rngs::OsRng
            .try_fill_bytes(&mut nonce_bytes)
            .expect("OS RNG must succeed");
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ct = self
            .cipher
            .encrypt(nonce, plain.as_bytes())
            .expect("ChaCha20-Poly1305 encryption cannot fail with a fresh nonce");
        let mut combined = Vec::with_capacity(12 + ct.len());
        combined.extend_from_slice(&nonce_bytes);
        combined.extend_from_slice(&ct);
        format!("v1:{}", BASE64_STANDARD.encode(&combined))
    }

    /// Store a deliberately non-secret value as `plain:<value>`.
    pub fn seal_plain(&self, plain: &str) -> String {
        format!("plain:{plain}")
    }

    /// `seal` when `secret`, else `seal_plain`.
    pub fn store(&self, value: &str, secret: bool) -> String {
        if secret {
            self.seal(value)
        } else {
            self.seal_plain(value)
        }
    }

    /// Open a stored value: decrypt `v1:`, strip `plain:`, refuse the rest.
    pub fn open(&self, s: &str) -> Result<String, SealError> {
        if let Some(plain) = s.strip_prefix("plain:") {
            return Ok(plain.to_string());
        }
        let Some(b64) = s.strip_prefix("v1:") else {
            return Err(SealError::Unrecognised);
        };
        let raw = BASE64_STANDARD
            .decode(b64)
            .map_err(|_| SealError::InvalidEncoding)?;
        if raw.len() < 12 + 1 {
            return Err(SealError::InvalidFormat);
        }
        let (nonce_bytes, ct) = raw.split_at(12);
        let nonce = Nonce::from_slice(nonce_bytes);
        let pt = self
            .cipher
            .decrypt(nonce, ct)
            .map_err(|_| SealError::DecryptionFailed)?;
        String::from_utf8(pt).map_err(|_| SealError::InvalidUtf8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key() -> SecretKey {
        SecretKey::from_hex(&"ab".repeat(32)).expect("test key must parse")
    }

    #[test]
    #[allow(clippy::similar_names)] // reviewed: kept as-is (R150)
    fn seal_then_open_round_trips() {
        let sealer = Sealer::new(&test_key());
        let sealed = sealer.seal("hello-secret");
        assert!(
            sealed.starts_with("v1:"),
            "sealed value must carry v1: prefix"
        );
        let opened = sealer.open(&sealed).expect("open must succeed");
        assert_eq!(opened, "hello-secret");
    }

    #[test]
    #[allow(clippy::similar_names)] // reviewed: kept as-is (R150)
    fn open_rejects_tampered() {
        let sealer = Sealer::new(&test_key());
        let sealed = sealer.seal("tamper-me");
        // Flip the last base64 char to corrupt the ciphertext.
        let mut tampered = sealed.clone();
        let last = tampered.pop().expect("sealed must be non-empty");
        let replacement = if last == 'A' { 'B' } else { 'A' };
        tampered.push(replacement);
        assert_ne!(sealed, tampered);
        let err = sealer
            .open(&tampered)
            .expect_err("tampered value must fail");
        assert_ne!(format!("{err}"), "");
    }

    #[test]
    fn open_strips_plain_prefix() {
        let sealer = Sealer::new(&test_key());
        let stored = sealer.seal_plain("https://example.com");
        assert_eq!(stored, "plain:https://example.com");
        assert_eq!(sealer.open(&stored).unwrap(), "https://example.com");
        assert_eq!(sealer.open("plain:").unwrap(), "");
    }

    #[test]
    fn open_rejects_unprefixed() {
        let sealer = Sealer::new(&test_key());
        for raw in ["postgres://plain-value", "", "v2:abc", "V1:abc"] {
            assert!(
                matches!(sealer.open(raw), Err(SealError::Unrecognised)),
                "{raw:?} must be refused"
            );
        }
    }

    #[test]
    fn key_is_derived_with_label() {
        // The cipher key is SHA-256("setu-seal-v1" || key), never the raw
        // key: a cipher over the raw bytes must not open our output.
        let key = test_key();
        let sealed = Sealer::new(&key).seal("derived");
        let raw = ChaCha20Poly1305::new(Key::from_slice(key.bytes()));
        let bytes = BASE64_STANDARD
            .decode(sealed.strip_prefix("v1:").unwrap())
            .unwrap();
        let (nonce, ct) = bytes.split_at(12);
        assert!(raw.decrypt(Nonce::from_slice(nonce), ct).is_err());
    }

    #[test]
    fn different_keys_do_not_open() {
        let a = Sealer::new(&SecretKey::from_hex(&"11".repeat(32)).unwrap());
        let b = Sealer::new(&SecretKey::from_hex(&"22".repeat(32)).unwrap());
        let sealed = a.seal("cross-key");
        let err = b.open(&sealed).expect_err("other key must not open");
        let _ = format!("{err}");
    }
}
