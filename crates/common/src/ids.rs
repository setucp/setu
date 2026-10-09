//! ULID identifiers.
//!
//! IDs are lowercase ULID strings. [`short_id`] is the last 8 chars, used in
//! Postgres `site_<short>` names.

/// New random lowercase ULID.
pub fn new_id() -> String {
    ulid::Ulid::new().to_string().to_lowercase()
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
    fn short_id_takes_last_8_chars() {
        assert_eq!(short_id("01JABCDEFGHIJKLMNOPQRSTUVWX"), "QRSTUVWX");
        assert_eq!(short_id("abc"), "abc");
    }
}
