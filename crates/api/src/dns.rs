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
//!
//! The control plane's resolver can see a different answer from the public
//! one (split horizon, a cache holding the old record); the findings are
//! what the control plane sees.

use std::net::IpAddr;
use std::sync::LazyLock;
use std::time::Duration;

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
    /// Why the CAA lookup could not be completed (SERVFAIL, a timeout, a
    /// DNSSEC failure), when it could not. A CA treats a failed CAA lookup
    /// as a refusal, so this is never reported as "no CAA record".
    pub caa_error: Option<String>,
}

/// Looks a name up in DNS.
#[async_trait]
pub trait DomainResolver: Send + Sync + std::fmt::Debug {
    async fn lookup(&self, domain: &str) -> DnsAnswers;
}

/// One deadline for a whole lookup: the CAA climb asks one query per label
/// in turn, and a name can have over a hundred labels.
const LOOKUP_DEADLINE: Duration = Duration::from_secs(10);

/// The resolver the control plane uses: the system's name servers. The
/// resolver is built once, on the first lookup, not on every request.
#[derive(Debug, Default)]
pub struct SystemResolver {
    resolver: std::sync::OnceLock<Result<hickory_resolver::TokioResolver, String>>,
}

impl SystemResolver {
    fn get(&self) -> Result<&hickory_resolver::TokioResolver, &str> {
        self.resolver
            .get_or_init(|| {
                hickory_resolver::TokioResolver::builder_tokio()
                    .map_err(|e| e.to_string())
                    .and_then(|b| b.build().map_err(|e| e.to_string()))
            })
            .as_ref()
            .map_err(String::as_str)
    }
}

#[async_trait]
impl DomainResolver for SystemResolver {
    async fn lookup(&self, domain: &str) -> DnsAnswers {
        let resolver = match self.get() {
            Ok(r) => r,
            Err(e) => return failed(e),
        };
        let mut out = DnsAnswers::default();
        let work = lookup_all(resolver, domain, &mut out);
        if tokio::time::timeout(LOOKUP_DEADLINE, work).await.is_err() {
            let why = format!(
                "the lookup took longer than {} s",
                LOOKUP_DEADLINE.as_secs()
            );
            if out.addresses.is_empty() && out.error.is_none() {
                out.error = Some(why);
            } else if out.caa.is_empty() && out.caa_error.is_none() {
                out.caa_error = Some(why);
            }
        }
        out
    }
}

async fn lookup_all(
    resolver: &hickory_resolver::TokioResolver,
    domain: &str,
    out: &mut DnsAnswers,
) {
    use hickory_resolver::proto::rr::{RData, RecordType};

    match resolver.lookup_ip(format!("{domain}.")).await {
        Ok(ips) => out.addresses = ips.iter().collect(),
        Err(e) => out.error = Some(e.to_string()),
    }
    // RFC 8659 §3: climb from the name towards the root and stop at the
    // first label set that has any CAA record. The bare TLD is never asked
    // (a deliberate shortcut: the climb should end at the root, but a CAA
    // record on a TLD is not something an operator can have set for us).
    let mut name = domain;
    loop {
        match resolver.lookup(format!("{name}."), RecordType::CAA).await {
            Ok(found) => {
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
            // "No records" is an answer; anything else is a failed lookup.
            Err(e) if e.is_no_records_found() => {}
            Err(e) => {
                out.caa_error = Some(format!("the CAA lookup for {name} failed: {e}"));
                break;
            }
        }
        match name.split_once('.') {
            Some((_, parent)) if parent.contains('.') => name = parent,
            _ => break,
        }
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

static CLOUDFLARE_NETS: LazyLock<Vec<IpNet>> =
    LazyLock::new(|| CLOUDFLARE.iter().filter_map(|n| n.parse().ok()).collect());

/// Is the address one of Cloudflare's proxy addresses?
#[must_use]
pub fn is_cloudflare(ip: IpAddr) -> bool {
    CLOUDFLARE_NETS.iter().any(|net| net.contains(&ip))
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

/// Every address of the name must be the server's: Let's Encrypt prefers
/// IPv6, so a stale AAAA at an old host fails issuance although the A
/// record is right. A server reports one public address, so an address of
/// the other family is a warning that names it; a different address of the
/// same family is an error.
fn points_at(addresses: &[IpAddr], server: IpAddr) -> Finding {
    let join = |v: &[&IpAddr]| {
        v.iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let others: Vec<&IpAddr> = addresses.iter().filter(|a| **a != server).collect();
    let same_family: Vec<&IpAddr> = others
        .iter()
        .copied()
        .filter(|a| a.is_ipv4() == server.is_ipv4())
        .collect();
    if !addresses.contains(&server) || !same_family.is_empty() {
        finding(
            "points_at_server",
            Level::Error,
            format!(
                "points at {}, not only at the server ({server}); certificate issuance will fail until the record is changed",
                join(&addresses.iter().collect::<Vec<_>>())
            ),
        )
    } else if others.is_empty() {
        finding(
            "points_at_server",
            Level::Ok,
            format!("points at the server ({server})"),
        )
    } else {
        finding(
            "points_at_server",
            Level::Warn,
            format!(
                "points at the server ({server}) and also at {}, which the server does not report; \
                 Let's Encrypt may validate over that address, so remove it unless it is this server",
                join(&others)
            ),
        )
    }
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
                Some(ip) => out.push(points_at(&answers.addresses, ip)),
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
    if let Some(why) = &answers.caa_error {
        out.push(finding(
            "caa",
            Level::Warn,
            format!("the CAA records could not be checked ({why}); a CA treats a failed CAA lookup as a refusal"),
        ));
    } else if caa_allows_lets_encrypt(&answers.caa) {
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
            ..DnsAnswers::default()
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
    fn a_stale_aaaa_next_to_the_right_a_record_is_not_all_ok() {
        let f = assess(
            &answers(&["203.0.113.9", "2001:db8::1"], vec![]),
            Some(ip("203.0.113.9")),
        );
        assert_eq!(level(&f, "points_at_server"), Some(Level::Warn));
        let m = &f
            .iter()
            .find(|x| x.id == "points_at_server")
            .unwrap()
            .message;
        assert!(m.contains("2001:db8::1"));
    }

    #[test]
    fn a_second_address_of_the_same_family_is_an_error() {
        let f = assess(
            &answers(&["203.0.113.9", "198.51.100.1"], vec![]),
            Some(ip("203.0.113.9")),
        );
        assert_eq!(level(&f, "points_at_server"), Some(Level::Error));
    }

    #[test]
    fn a_failed_caa_lookup_is_not_reported_as_no_record() {
        let a = DnsAnswers {
            caa_error: Some("SERVFAIL".into()),
            ..answers(&["203.0.113.9"], vec![])
        };
        let f = assess(&a, Some(ip("203.0.113.9")));
        assert_eq!(level(&f, "caa"), Some(Level::Warn));
        assert!(f
            .iter()
            .find(|x| x.id == "caa")
            .unwrap()
            .message
            .contains("SERVFAIL"));
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
