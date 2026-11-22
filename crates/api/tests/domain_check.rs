// Test crate: a panic is the failure report (R146).
#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `GET /api/v1/domain-check`: the DNS pre-check, answered from a fake
//! resolver so no test touches the network.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::Value;
use setu_api::dns::{CaaRecord, DnsAnswers, DomainResolver};
use setu_api::testing::{TestOptions, TestServer};
use setu_jobs::testing::FakeAgent;
use setu_testkit::TestDb;

#[derive(Debug, Default)]
struct FakeDns(Mutex<HashMap<String, DnsAnswers>>);

#[async_trait]
impl DomainResolver for FakeDns {
    async fn lookup(&self, domain: &str) -> DnsAnswers {
        self.0
            .lock()
            .unwrap()
            .get(domain)
            .cloned()
            .unwrap_or_else(|| DnsAnswers {
                error: Some("no records found".into()),
                ..DnsAnswers::default()
            })
    }
}

async fn server_with_ip(srv: &TestServer, ip: Option<&str>) -> String {
    let (_p, token) = srv
        .db
        .servers()
        .create_pending("web1", false)
        .await
        .unwrap();
    let input = setu_db::JoinInput {
        nonce: String::new(),
        hostname: "web1".into(),
        public_ip: ip.map(str::to_string),
        arch: "x86_64".into(),
        os: "linux".into(),
        cpu_cores: 4,
        memory_mb: 4096,
        disk_mb: 102_400,
    };
    let key = setu_common::SecretKey::from_hex(&"ab".repeat(32)).unwrap();
    let (server, _) = srv
        .db
        .servers()
        .join(&token, &input, &setu_common::Sealer::new(&key))
        .await
        .unwrap();
    server.id
}

async fn boot(dns: Arc<FakeDns>) -> TestServer {
    let opts = TestOptions {
        resolver: Some(dns),
        ..TestOptions::default()
    };
    TestServer::spawn_with(TestDb::new().await, FakeAgent::new(), opts).await
}

async fn get(
    srv: &TestServer,
    client: &reqwest::Client,
    domain: &str,
    server: &str,
) -> (u16, Value) {
    let resp = client
        .get(srv.url("/api/v1/domain-check"))
        .query(&[("domain", domain), ("server_id", server)])
        .send()
        .await
        .unwrap();
    (resp.status().as_u16(), resp.json().await.unwrap())
}

fn level(body: &Value, id: &str) -> String {
    body["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["id"] == id)
        .map_or_else(String::new, |f| f["level"].as_str().unwrap().to_string())
}

#[tokio::test]
async fn a_domain_pointing_at_the_server_is_ok() {
    let dns = Arc::new(FakeDns::default());
    dns.0.lock().unwrap().insert(
        "example.com".into(),
        DnsAnswers {
            addresses: vec!["203.0.113.9".parse().unwrap()],
            ..DnsAnswers::default()
        },
    );
    let srv = boot(dns).await;
    let owner = srv.login_as_owner().await;
    let server = server_with_ip(&srv, Some("203.0.113.9")).await;
    let (status, body) = get(&srv, &owner, "Example.com.", &server).await;
    assert_eq!(status, 200);
    assert_eq!(body["domain"], "example.com");
    assert_eq!(body["server_ip"], "203.0.113.9");
    assert_eq!(body["ok"], true);
    assert_eq!(level(&body, "points_at_server"), "ok");
}

#[tokio::test]
async fn wrong_address_unresolved_name_and_caa_are_errors() {
    let dns = Arc::new(FakeDns::default());
    dns.0.lock().unwrap().insert(
        "elsewhere.example".into(),
        DnsAnswers {
            addresses: vec!["198.51.100.1".parse().unwrap()],
            caa: vec![CaaRecord {
                tag: "issue".into(),
                value: "digicert.com".into(),
            }],
            error: None,
        },
    );
    let srv = boot(dns).await;
    let owner = srv.login_as_owner().await;
    let server = server_with_ip(&srv, Some("203.0.113.9")).await;
    let (_, body) = get(&srv, &owner, "elsewhere.example", &server).await;
    assert_eq!(body["ok"], false);
    assert_eq!(level(&body, "points_at_server"), "error");
    assert_eq!(level(&body, "caa"), "error");
    let (status, body) = get(&srv, &owner, "missing.example", &server).await;
    assert_eq!(
        status, 200,
        "an unresolved name is a finding, not an HTTP error"
    );
    assert_eq!(body["ok"], false);
    assert_eq!(level(&body, "dns"), "error");
}

#[tokio::test]
async fn a_server_without_a_public_address_only_warns() {
    let dns = Arc::new(FakeDns::default());
    dns.0.lock().unwrap().insert(
        "example.com".into(),
        DnsAnswers {
            addresses: vec!["198.51.100.1".parse().unwrap()],
            ..DnsAnswers::default()
        },
    );
    let srv = boot(dns).await;
    let owner = srv.login_as_owner().await;
    let server = server_with_ip(&srv, None).await;
    let (_, body) = get(&srv, &owner, "example.com", &server).await;
    assert_eq!(body["ok"], true);
    assert_eq!(level(&body, "points_at_server"), "warn");
}

#[tokio::test]
async fn bad_input_unknown_server_and_roles_are_refused() {
    let srv = boot(Arc::new(FakeDns::default())).await;
    let owner = srv.login_as_owner().await;
    let server = server_with_ip(&srv, None).await;
    let (status, _) = get(&srv, &owner, "not a host", &server).await;
    assert_eq!(status, 400);
    let (status, _) = get(&srv, &owner, "example.com", "nope").await;
    assert_eq!(status, 404);
    let viewer = srv.login_as("viewer").await;
    let (status, _) = get(&srv, &viewer, "example.com", &server).await;
    assert_eq!(status, 403);
    let anon = reqwest::Client::new();
    let (status, _) = get(&srv, &anon, "example.com", &server).await;
    assert_eq!(status, 401);
}
