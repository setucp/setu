//! `listen.rs` (plan Task 7, spec §8): the control plane on a unix socket.

mod common;

use std::os::unix::fs::{FileTypeExt, PermissionsExt};

use setu_api::testing::TestServer;
use setu_jobs::testing::FakeAgent;
use setu_testkit::TestDb;

#[tokio::test]
async fn unix_listener_serves_requests() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let dir = tempfile::tempdir().expect("tempdir");
    let sock = dir.path().join("http.sock");

    // A stale socket from a previous process is replaced.
    drop(std::os::unix::net::UnixListener::bind(&sock).expect("stale socket"));
    assert!(std::fs::symlink_metadata(&sock)
        .expect("stale socket exists")
        .file_type()
        .is_socket());
    let _serving = srv.serve_unix(&sock).await;

    let mode = std::fs::metadata(&sock)
        .expect("socket")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o660, "socket mode {mode:o}");

    let (status, _, body) =
        common::unix_request(&sock, "GET", "/healthz", &[], String::new()).await;
    assert_eq!(status, 200, "body: {body}");
    assert!(body.contains("ok"));
    let (status, _, body) =
        common::unix_request(&sock, "GET", "/api/v1/setup", &[], String::new()).await;
    assert_eq!(status, 200, "body: {body}");
    let (status, _, _) =
        common::unix_request(&sock, "GET", "/api/v1/auth/me", &[], String::new()).await;
    assert_eq!(status, 401);

    // Anything but a socket at the path is refused, never deleted.
    let file = dir.path().join("not-a-socket");
    std::fs::write(&file, b"keep me").expect("write file");
    let err = setu_api::listen::bind(&setu_common::Listen::Unix(file.clone()))
        .await
        .expect_err("a regular file is not replaced");
    assert!(err.to_string().contains("not a socket"), "{err}");
    assert_eq!(std::fs::read(&file).expect("still there"), b"keep me");
}
