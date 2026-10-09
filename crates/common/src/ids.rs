//! ULID identifiers.
//!
//! IDs are lowercase ULID strings. [`short_id`] is the last 8 chars, used in
//! user, unit, database and path names; [`is_short_id`] checks one.

/// New random lowercase ULID.
pub fn new_id() -> String {
    ulid::Ulid::new().to_string().to_lowercase()
}

/// `n` bytes from the OS-seeded thread RNG, hex-encoded (nonces).
pub fn random_hex(n: usize) -> String {
    let mut bytes = vec![0u8; n];
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut bytes);
    hex::encode(bytes)
}

/// Last 8 chars of an id (or the whole string when shorter).
pub fn short_id(id: &str) -> String {
    if id.len() <= 8 {
        id.to_string()
    } else {
        // Byte slicing can split a char boundary on non-ASCII input; ids are
        // always ASCII ULIDs, but stay correct for anything.
        id.chars()
            .rev()
            .take(8)
            .collect::<String>()
            .chars()
            .rev()
            .collect()
    }
}

/// `^[a-z0-9]{8}$`: the only site handle that crosses the sudo boundary.
pub fn is_short_id(s: &str) -> bool {
    s.len() == 8
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_id_is_lowercase_ulid() {
        let id = new_id();
        assert_eq!(id.len(), 26, "ULID must be 26 chars");
        assert_eq!(id, id.to_lowercase(), "id must be lowercase");
        assert!(
            id.chars().all(|c| c.is_ascii_alphanumeric()),
            "unexpected char in {id}"
        );
        assert_ne!(new_id(), new_id(), "ids must be unique");
    }

    #[test]
    fn is_short_id_checks_eight_lowercase_alnum() {
        assert!(is_short_id("abcd1234"));
        assert!(is_short_id(&short_id(&new_id())));
        assert!(!is_short_id("ABCD1234"));
        assert!(!is_short_id("abcd123"));
        assert!(!is_short_id("abcd12345"));
        assert!(!is_short_id("../abcde"));
    }

    #[test]
    fn short_id_takes_last_8_chars() {
        assert_eq!(short_id("01JABCDEFGHIJKLMNOPQRSTUVWX"), "QRSTUVWX");
        assert_eq!(short_id("abc"), "abc");
    }
}
