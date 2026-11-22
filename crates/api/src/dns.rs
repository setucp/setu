//! The DNS half of the domain pre-check: a resolver trait (tests swap in
//! a fake, like [`crate::follow::ManifestFetcher`]), the real resolver
//! (hickory, system configuration) and the pure assessment that turns the
//! answers into findings for the operator.
//!
//! The assessment answers one question before a site is provisioned: will
//! Caddy be able to get a Let's Encrypt certificate for this name on this
//! server? It checks that the name resolves, that the addresses are the
//! server's, that Cloudflare's proxy is not in the way unnoticed and that
//! no CAA record forbids Let's Encrypt (RFC 8659: the closest CAA record
//! set up the tree decides).

use std::net::IpAddr;

use async_trait::async_trait;
use ipnet::IpNet;
use serde::Serialize;
use utoipa::ToSchema;

/// The CAA issuer domain Caddy's default CA (Let's Encrypt) matches.
pub const LETS_ENCRYPT: &str = "letsencrypt.org";

/// One CAA record, reduced to what the assessment reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaaRecord {
    /// Property tag, lower-case (`issue`, `issuewild`, `iodef`, …).
    pub tag: String,
    /// Property value as text (`letsencrypt.org`, `;`, …).
    pub value: String,
}

/// What a lookup of one name found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DnsAnswers {
    /// A and AAAA records of the name.
    pub addresses: Vec<IpAddr>,
    /// The closest non-empty CAA record set up the tree (empty: none).
    pub caa: Vec<CaaRecord>,
    /// Why the address lookup failed, when it did (shown to the operator).
    pub error: Option<String>,
}

/// Looks a name up in DNS.
#[async_trait]
pub trait DomainResolver: Send + Sync + std::fmt::Debug {
    async fn lookup(&self, domain: &str) -> DnsAnswers;
}

/// The resolver the control plane uses: the system's name servers.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemResolver;

#[async_trait]
impl DomainResolver for SystemResolver {
    async fn lookup(&self, domain: &str) -> DnsAnswers {
        use hickory_resolver::proto::rr::{RData, RecordType};
        use hickory_resolver::TokioResolver;

        let built = TokioResolver::builder_tokio()
            .map_err(|e| e.to_string())
            .and_then(|b| b.build().map_err(|e| e.to_string()));
        let resolver = match built {
            Ok(r) => r,
            Err(e) => return failed(&e),
        };
        let mut out = DnsAnswers::default();
        match resolver.lookup_ip(format!("{domain}.")).await {
            Ok(ips) => out.addresses = ips.iter().collect(),
            Err(e) => out.error = Some(e.to_string()),
        }
        // RFC 8659 §3: climb from the name towards the root and stop at
        // the first label set that has any CAA record. The bare TLD is
        // never asked.
        let mut name = domain;
        loop {
            if let Ok(found) = resolver.lookup(format!("{name}."), RecordType::CAA).await {
                let records: Vec<CaaRecord> = found
                    .answers()
                    .iter()
                    .filter_map(|r| match &r.data {
                        RData::CAA(c) => Some(CaaRecord {
                            tag: c.tag.to_ascii_lowercase(),
                            value: String::from_utf8_lossy(&c.value).into_owned(),
                        }),
                        _ => None,
                    })
                    .collect();
                if !records.is_empty() {
                    out.caa = records;
                    break;
                }
            }
            match name.split_once('.') {
                Some((_, parent)) if parent.contains('.') => name = parent,
                _ => break,
            }
        }
        out
    }
}

fn failed(why: &str) -> DnsAnswers {
    DnsAnswers {
        error: Some(format!("the system DNS resolver is unavailable: {why}")),
        ..DnsAnswers::default()
    }
}

/// How serious one finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// Fine.
    Ok,
    /// Provisioning can work; the operator should know.
    Warn,
    /// Certificate issuance will fail until this is fixed.
    Error,
}

/// One finding of the pre-check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Finding {
    /// Stable id: `dns`, `points_at_server`, `proxy` or `caa`.
    pub id: &'static str,
    pub level: Level,
    pub message: String,
}

fn finding(id: &'static str, level: Level, message: impl Into<String>) -> Finding {
    Finding {
        id,
        level,
        message: message.into(),
    }
}

/// Cloudflare's published proxy ranges (cloudflare.com/ips).
const CLOUDFLARE: &[&str] = &[
    "173.245.48.0/20",
    "103.21.244.0/22",
    "103.22.200.0/22",
    "103.31.4.0/22",
    "141.101.64.0/18",
    "108.162.192.0/18",
    "190.93.240.0/20",
    "188.114.96.0/20",
    "197.234.240.0/22",
    "198.41.128.0/17",
    "162.158.0.0/15",
    "104.16.0.0/13",
    "104.24.0.0/14",
    "172.64.0.0/13",
    "131.0.72.0/22",
    "2400:cb00::/32",
    "2606:4700::/32",
    "2803:f800::/32",
    "2405:b500::/32",
    "2405:8100::/32",
    "2a06:98c0::/29",
    "2c0f:f248::/32",
];

/// Is the address one of Cloudflare's proxy addresses?
#[must_use]
pub fn is_cloudflare(ip: IpAddr) -> bool {
    CLOUDFLARE
        .iter()
        .filter_map(|n| n.parse::<IpNet>().ok())
        .any(|net| net.contains(&ip))
}

/// Does the CAA record set allow Let's Encrypt to issue for the name
/// itself? No record set, or one without `issue` records, allows it.
#[must_use]
pub fn caa_allows_lets_encrypt(caa: &[CaaRecord]) -> bool {
    let mut issue = caa.iter().filter(|r| r.tag == "issue").peekable();
    if issue.peek().is_none() {
        return true;
    }
    issue.any(|r| {
        // `letsencrypt.org; accounturi=…`: the issuer is the part before `;`.
        let issuer = r.value.split(';').next().unwrap_or("").trim();
        issuer.eq_ignore_ascii_case(LETS_ENCRYPT)
    })
}

/// Turn what DNS said into findings. `server_ip` is the public address
/// the chosen server reported (none until its agent has said so).
#[must_use]
pub fn assess(answers: &DnsAnswers, server_ip: Option<IpAddr>) -> Vec<Finding> {
    let mut out = Vec::new();
    if answers.addresses.is_empty() {
        let why = answers
            .error
            .as_deref()
            .map(|e| format!(" ({e})"))
            .unwrap_or_default();
        out.push(finding(
            "dns",
            Level::Error,
            format!("the name has no A or AAAA record{why}; add one that points at the server"),
        ));
    } else {
        let list: Vec<String> = answers.addresses.iter().map(ToString::to_string).collect();
        out.push(finding(
            "dns",
            Level::Ok,
            format!("resolves to {}", list.join(", ")),
        ));
        let proxied: Vec<&IpAddr> = answers
            .addresses
            .iter()
            .filter(|ip| is_cloudflare(**ip))
            .collect();
        if proxied.is_empty() {
            match server_ip {
                None => out.push(finding(
                    "points_at_server",
                    Level::Warn,
                    "the server has not reported its public address yet, so the record cannot be compared",
                )),
                Some(ip) if answers.addresses.contains(&ip) => out.push(finding(
                    "points_at_server",
                    Level::Ok,
                    format!("points at the server ({ip})"),
                )),
                Some(ip) => out.push(finding(
                    "points_at_server",
                    Level::Error,
                    format!(
                        "points at {}, not at the server ({ip}); certificate issuance will fail until the record is changed",
                        answers.addresses.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
                    ),
                )),
            }
        } else {
            out.push(finding(
                "proxy",
                Level::Warn,
                "the name is proxied by Cloudflare, so the server's address is hidden from the check; \
                 set the SSL mode to Full (strict), or turn the proxy off until the certificate is issued",
            ));
        }
    }
    if caa_allows_lets_encrypt(&answers.caa) {
        out.push(finding(
            "caa",
            Level::Ok,
            if answers.caa.is_empty() {
                "no CAA record restricts issuance"
            } else {
                "the CAA record allows Let's Encrypt"
            },
        ));
    } else {
        out.push(finding(
            "caa",
            Level::Error,
            format!("a CAA record does not list {LETS_ENCRYPT}; add `0 issue \"{LETS_ENCRYPT}\"`"),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn caa(tag: &str, value: &str) -> CaaRecord {
        CaaRecord {
            tag: tag.into(),
            value: value.into(),
        }
    }

    fn answers(addrs: &[&str], caa: Vec<CaaRecord>) -> DnsAnswers {
        DnsAnswers {
            addresses: addrs.iter().map(|a| ip(a)).collect(),
            caa,
            error: None,
        }
    }

    fn level(findings: &[Finding], id: &str) -> Option<Level> {
        findings.iter().find(|f| f.id == id).map(|f| f.level)
    }

    #[test]
    fn cloudflare_ranges_match_both_families() {
        assert!(is_cloudflare(ip("104.16.1.1")));
        assert!(is_cloudflare(ip("172.67.3.4")));
        assert!(is_cloudflare(ip("2606:4700::1")));
        assert!(!is_cloudflare(ip("203.0.113.9")));
        assert!(!is_cloudflare(ip("2001:db8::1")));
    }

    #[test]
    fn caa_without_issue_records_allows_issuance() {
        assert!(caa_allows_lets_encrypt(&[]));
        assert!(caa_allows_lets_encrypt(&[caa("iodef", "mailto:a@b.c")]));
        assert!(caa_allows_lets_encrypt(&[caa("issuewild", ";")]));
    }

    #[test]
    fn caa_issue_must_name_lets_encrypt() {
        assert!(caa_allows_lets_encrypt(&[caa("issue", "letsencrypt.org")]));
        assert!(caa_allows_lets_encrypt(&[
            caa("issue", "digicert.com"),
            caa("issue", "LetsEncrypt.org; accounturi=https://x/1"),
        ]));
        assert!(!caa_allows_lets_encrypt(&[caa("issue", "digicert.com")]));
        assert!(!caa_allows_lets_encrypt(&[caa("issue", ";")]));
        assert!(!caa_allows_lets_encrypt(&[caa(
            "issue",
            "notletsencrypt.org"
        )]));
    }

    #[test]
    fn a_name_that_points_at_the_server_passes() {
        let f = assess(&answers(&["203.0.113.9"], vec![]), Some(ip("203.0.113.9")));
        assert_eq!(level(&f, "dns"), Some(Level::Ok));
        assert_eq!(level(&f, "points_at_server"), Some(Level::Ok));
        assert_eq!(level(&f, "caa"), Some(Level::Ok));
        assert!(f.iter().all(|x| x.level == Level::Ok));
    }

    #[test]
    fn a_name_that_points_elsewhere_is_an_error() {
        let f = assess(&answers(&["198.51.100.1"], vec![]), Some(ip("203.0.113.9")));
        assert_eq!(level(&f, "points_at_server"), Some(Level::Error));
    }

    #[test]
    fn an_unknown_server_address_is_a_warning() {
        let f = assess(&answers(&["198.51.100.1"], vec![]), None);
        assert_eq!(level(&f, "points_at_server"), Some(Level::Warn));
    }

    #[test]
    fn an_unresolved_name_is_an_error_with_the_reason() {
        let a = DnsAnswers {
            error: Some("no records found".into()),
            ..DnsAnswers::default()
        };
        let f = assess(&a, Some(ip("203.0.113.9")));
        assert_eq!(level(&f, "dns"), Some(Level::Error));
        assert!(f[0].message.contains("no records found"));
        assert_eq!(level(&f, "points_at_server"), None);
    }

    #[test]
    fn a_cloudflare_proxied_name_warns_instead_of_failing() {
        let f = assess(&answers(&["104.21.1.2"], vec![]), Some(ip("203.0.113.9")));
        assert_eq!(level(&f, "proxy"), Some(Level::Warn));
        assert_eq!(level(&f, "points_at_server"), None);
    }

    #[test]
    fn a_caa_record_that_excludes_lets_encrypt_is_an_error() {
        let f = assess(
            &answers(&["203.0.113.9"], vec![caa("issue", "digicert.com")]),
            Some(ip("203.0.113.9")),
        );
        assert_eq!(level(&f, "caa"), Some(Level::Error));
    }
}
