//! Shared helpers for the api integration tests.

#![allow(dead_code)]

use std::path::Path;

use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;

/// One HTTP/1.1 request over a unix socket (hyper, no reqwest support):
/// returns status, response headers and body text.
pub async fn unix_request(
    socket: &Path,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: String,
) -> (u16, hyper::HeaderMap, String) {
    let stream = tokio::net::UnixStream::connect(socket)
        .await
        .expect("connect to the unix socket");
    let (mut sender, conn) =
        hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream))
            .await
            .expect("http1 handshake");
    tokio::spawn(async move {
        let _ = conn.await;
    });
    let mut req = hyper::Request::builder()
        .method(method)
        .uri(path)
        .header("host", "panel.local");
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let resp = sender
        .send_request(req.body(Full::new(Bytes::from(body))).expect("request"))
        .await
        .expect("send over the unix socket");
    let status = resp.status().as_u16();
    let headers = resp.headers().clone();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("read body")
        .to_bytes();
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}
