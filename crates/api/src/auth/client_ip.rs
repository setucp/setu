//! The client's IP for the login limiter and audit rows (spec §7).
//!
//! On the unix socket the peer is Caddy by construction (the socket is
//! `0660`, group `caddy`), so `X-Forwarded-For` is believed. On TCP it is
//! believed only when the peer is inside `trusted_proxies`; otherwise the
//! peer address is the client. Within a believed header the client is the
//! first hop, reading from the right, that is not itself a trusted proxy
//! (a client cannot hide behind hops it prepended itself). An unparsable
//! hop ends the walk: the header is ignored.

use std::net::IpAddr;

use axum::{
    extract::{ConnectInfo, FromRequestParts},
    http::{request::Parts, HeaderMap},
};
use ipnet::IpNet;
use setu_common::AppError;

use crate::listen::Peer;
use crate::state::AppState;

/// The client address of a request (see the module docs).
pub fn client_ip(peer: &Peer, headers: &HeaderMap, trusted: &[IpNet]) -> Option<IpAddr> {
    match peer {
        Peer::Unix => from_forwarded_for(headers, trusted),
        Peer::Tcp(ip) if is_trusted(ip, trusted) => {
            from_forwarded_for(headers, trusted).or(Some(*ip))
        }
        Peer::Tcp(ip) => Some(*ip),
    }
}

fn is_trusted(ip: &IpAddr, trusted: &[IpNet]) -> bool {
    let ip = ip.to_canonical();
    trusted.iter().any(|net| net.contains(&ip))
}

fn from_forwarded_for(headers: &HeaderMap, trusted: &[IpNet]) -> Option<IpAddr> {
    let hops: Vec<&str> = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .collect();
    let mut leftmost = None;
    for hop in hops.iter().rev() {
        let ip = parse_hop(hop)?;
        if !is_trusted(&ip, trusted) {
            return Some(ip);
        }
        leftmost = Some(ip);
    }
    leftmost
}

/// `203.0.113.9`, `2001:db8::1` or `[2001:db8::1]`.
fn parse_hop(hop: &str) -> Option<IpAddr> {
    let bare = hop
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(hop);
    bare.parse::<IpAddr>().ok().map(|ip| ip.to_canonical())
}

/// Extractor: the request's client IP (`None` when unknown, e.g. a unix
/// peer without `X-Forwarded-For`).
#[derive(Debug, Clone, Default)]
pub struct ClientIp {
    ip: Option<IpAddr>,
    text: Option<String>,
}

impl ClientIp {
    pub fn new(ip: Option<IpAddr>) -> Self {
        Self {
            ip,
            text: ip.map(|ip| ip.to_string()),
        }
    }

    /// The address, if known.
    pub fn ip(&self) -> Option<IpAddr> {
        self.ip
    }

    /// The limiter key: unknown clients share one bucket.
    pub fn limiter_key(&self) -> IpAddr {
        self.ip
            .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED))
    }

    /// The text form for `audit_log.ip`.
    pub fn as_deref(&self) -> Option<&str> {
        self.text.as_deref()
    }
}

impl FromRequestParts<AppState> for ClientIp {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let peer = parts
            .extensions
            .get::<ConnectInfo<Peer>>()
            .map(|ConnectInfo(peer)| *peer);
        Ok(Self::new(peer.and_then(|peer| {
            client_ip(&peer, &parts.headers, &state.config.trusted_proxies)
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xff(values: &[&str]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for v in values {
            h.append("x-forwarded-for", v.parse().unwrap());
        }
        h
    }

    fn nets(list: &[&str]) -> Vec<IpNet> {
        list.iter().map(|n| n.parse().unwrap()).collect()
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn unix_peer_believes_forwarded_for() {
        let trusted = nets(&["127.0.0.1/32"]);
        assert_eq!(
            client_ip(&Peer::Unix, &xff(&["203.0.113.9"]), &trusted),
            Some(ip("203.0.113.9"))
        );
        // A forged left part is skipped: the rightmost untrusted hop wins.
        assert_eq!(
            client_ip(&Peer::Unix, &xff(&["1.2.3.4, 203.0.113.9"]), &trusted),
            Some(ip("203.0.113.9"))
        );
        // Trusted hops on the right are skipped.
        assert_eq!(
            client_ip(&Peer::Unix, &xff(&["198.51.100.7", "127.0.0.1"]), &trusted),
            Some(ip("198.51.100.7"))
        );
        assert_eq!(client_ip(&Peer::Unix, &HeaderMap::new(), &trusted), None);
        assert_eq!(client_ip(&Peer::Unix, &xff(&["not-an-ip"]), &trusted), None);
        assert_eq!(
            client_ip(&Peer::Unix, &xff(&["[2001:db8::1]"]), &trusted),
            Some(ip("2001:db8::1"))
        );
    }

    #[test]
    fn tcp_peer_believes_forwarded_for_only_from_trusted_proxies() {
        let trusted = nets(&["10.0.0.0/8"]);
        let proxy = Peer::Tcp(ip("10.1.2.3"));
        let stranger = Peer::Tcp(ip("192.0.2.50"));
        assert_eq!(
            client_ip(&proxy, &xff(&["203.0.113.9"]), &trusted),
            Some(ip("203.0.113.9"))
        );
        assert_eq!(
            client_ip(&proxy, &HeaderMap::new(), &trusted),
            Some(ip("10.1.2.3"))
        );
        assert_eq!(
            client_ip(&stranger, &xff(&["203.0.113.9"]), &trusted),
            Some(ip("192.0.2.50")),
            "untrusted peer's header is ignored"
        );
        // A v4-mapped v6 peer counts as its v4 address.
        assert_eq!(
            client_ip(
                &Peer::Tcp(ip("::ffff:10.0.0.1")),
                &xff(&["203.0.113.9"]),
                &trusted
            ),
            Some(ip("203.0.113.9"))
        );
        // Every hop trusted: the leftmost one.
        assert_eq!(
            client_ip(&proxy, &xff(&["10.9.9.9, 10.8.8.8"]), &trusted),
            Some(ip("10.9.9.9"))
        );
    }
}
