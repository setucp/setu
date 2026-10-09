//! Sealing with ChaCha20-Poly1305 (`v1:` scheme).
//!
//! [`Sealer::seal`] produces `v1:<base64 nonce12 || ciphertext>` with a fresh
//! random 12-byte nonce. [`Sealer::open`] decrypts `v1:` values and returns
//! anything without the prefix as-is (plain values are never sealed).

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

use crate::config::SecretKey;

/// Encrypts and decrypts secret values.
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
}

impl Sealer {
    pub fn new(secret: &SecretKey) -> Self {
        let key = Key::from_slice(secret.bytes());
        Self {
            cipher: ChaCha20Poly1305::new(key),
        }
    }

    /// Seal `plain` as `v1:<base64 nonce || ciphertext>`.
    ///
    /// Infallible by design (the plan fixes this signature): the nonce comes
    /// from the OS RNG and encryption with a valid key and fresh nonce cannot
    /// fail, so the two `expect`s below fire only on OS-level entropy failure.
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

    /// Open a sealed value, or return plain values unchanged.
    pub fn open(&self, s: &str) -> Result<String, SealError> {
        let Some(b64) = s.strip_prefix("v1:") else {
            return Ok(s.to_string());
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
    fn open_rejects_tampered_ciphertext() {
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
        let msg = format!("{err}");
        assert!(!msg.is_empty());
    }

    #[test]
    fn open_passes_plain_values_through() {
        let sealer = Sealer::new(&test_key());
        let plain = "postgres://plain-value";
        let opened = sealer.open(plain).expect("plain must pass through");
        assert_eq!(opened, plain);
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
