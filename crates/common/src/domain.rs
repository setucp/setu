//! Site domain normalisation (spec §3): lowercase, IDNA ASCII, no
//! wildcard, not an IP, no port or path, DNS label and length limits.

use std::net::IpAddr;

/// Why a domain was refused (message is safe to show to operators).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid domain: {0}")]
pub struct DomainError(pub String);

fn err(msg: impl Into<String>) -> DomainError {
    DomainError(msg.into())
}

/// Normalise an operator-supplied domain to its stored form.
pub fn normalise(input: &str) -> Result<String, DomainError> {
    let trimmed = input.trim();
    let trimmed = trimmed.strip_suffix('.').unwrap_or(trimmed);
    if trimmed.is_empty() {
        return Err(err("domain is empty"));
    }
    if trimmed.contains('*') {
        return Err(err("wildcards are not allowed"));
    }
    if trimmed.contains(':') || trimmed.starts_with('[') {
        return Err(err("ports, schemes and IPv6 addresses are not allowed"));
    }
    if trimmed.contains(['/', '?', '#', '@', '\\']) {
        return Err(err("paths are not allowed"));
    }
    if trimmed.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(err("whitespace is not allowed"));
    }
    if trimmed.parse::<IpAddr>().is_ok() {
        return Err(err("IP addresses are not allowed"));
    }
    let ascii = idna::domain_to_ascii(trimmed).map_err(|_| err("not a valid IDNA name"))?;
    if ascii.len() > 253 {
        return Err(err("longer than 253 characters"));
    }
    let labels: Vec<&str> = ascii.split('.').collect();
    if labels.len() < 2 {
        return Err(err("needs at least two labels"));
    }
    for label in &labels {
        if label.is_empty() || label.len() > 63 {
            return Err(err("each label must be 1 to 63 characters"));
        }
        let ok_chars = label
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if !ok_chars || label.starts_with('-') || label.ends_with('-') {
            return Err(err("labels use a-z, 0-9 and inner hyphens only"));
        }
    }
    if labels
        .last()
        .is_some_and(|tld| tld.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(err("IP addresses are not allowed"));
    }
    Ok(ascii)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalise_lowercases_and_idna() {
        assert_eq!(normalise("  Example.COM ").unwrap(), "example.com");
        assert_eq!(
            normalise("Bücher.example").unwrap(),
            "xn--bcher-kva.example"
        );
        assert_eq!(normalise("a.b.example.org.").unwrap(), "a.b.example.org");
    }

    #[test]
    fn normalise_rejects_wildcard_ip_and_port() {
        for bad in [
            "*.example.com",
            "127.0.0.1",
            "10.0.0.1",
            "[::1]",
            "::1",
            "example.com:8080",
            "example.com/path",
            "localhost",
            "",
            "exa mple.com",
            "http://example.com",
        ] {
            assert!(normalise(bad).is_err(), "{bad:?} must be rejected");
        }
        let long_label = format!("{}.com", "a".repeat(64));
        assert!(normalise(&long_label).is_err());
        let long_total = format!("{}.com", vec!["a".repeat(60); 5].join("."));
        assert!(normalise(&long_total).is_err());
    }
}
