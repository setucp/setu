// Test crate: a panic is the failure report (R146).
#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Join: one nonce per install, reused by the retry after a network
//! error (spec §5).

use std::sync::{Arc, Mutex};

use setu_proto::JoinRequest;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Read one HTTP/1.1 request (headers + Content-Length body).
async fn read_request(sock: &mut tokio::net::TcpStream) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = sock.read(&mut chunk).await.expect("read");
        if n == 0 {
            return buf;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
            let len: usize = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
            while buf.len() < end + 4 + len {
                let n = sock.read(&mut chunk).await.expect("read body");
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            return buf[end + 4..].to_vec();
        }
    }
}

#[tokio::test]
async fn join_reuses_one_nonce_across_retries() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    let nonces = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = nonces.clone();
    tokio::spawn(async move {
        for attempt in 0..2 {
            let (mut sock, _) = listener.accept().await.expect("accept");
            let body = read_request(&mut sock).await;
            let req: serde_json::Value = serde_json::from_slice(&body).expect("join json");
            seen.lock()
                .expect("nonces")
                .push(req["nonce"].as_str().unwrap_or_default().to_string());
            if attempt == 0 {
                // The response is lost: close without answering.
                drop(sock);
                continue;
            }
            let resp = r#"{"server_id":"srv1","server_token":"setu_srv_tok"}"#;
            let msg = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{resp}",
                resp.len()
            );
            sock.write_all(msg.as_bytes()).await.expect("write");
        }
    });

    let request = JoinRequest {
        join_token: "setu_join_x".into(),
        nonce: setu_common::random_hex(16),
        hostname: "h".into(),
        arch: "x86_64".into(),
        os: "linux".into(),
        cpu_cores: 1,
        memory_mb: 1,
        disk_mb: 1,
    };
    let client = reqwest::Client::new();
    let joined = setu_agent::install::join_request(&client, &base, &request)
        .await
        .expect("joined on the retry");
    assert_eq!(joined.server_id, "srv1");
    let nonces = nonces.lock().expect("nonces").clone();
    assert_eq!(nonces.len(), 2, "one retry");
    assert_eq!(nonces[0], nonces[1], "same nonce on the retry");
    assert_eq!(nonces[0], request.nonce);
}
