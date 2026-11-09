// Test crate: a panic is the failure report (R146).
#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `route_update` over [`FakeAgent`]: the route carries the stored
//! credentials, and a site that is not settled is refused.

mod common;

use common::{ctx_with_logs, manifest_toml, setup};
use setu_jobs::{provision, route_update, testing::FakeAgent};

#[tokio::test]
async fn the_route_carries_the_stored_credentials_then_none() {
    let (_test_db, fx) = setup(&manifest_toml("0.1.0")).await;
    let agent = FakeAgent::new();
    let (ctx, logs) = ctx_with_logs(fx.db.clone(), &agent);
    provision::run(&ctx, &fx.site_id).await.expect("provision");
    logs.lock().unwrap().clear();

    let hash = format!("$2b$12${}", "a".repeat(53));
    fx.db
        .basic_auths()
        .set(&fx.site_id, "alice", &hash, chrono::Utc::now())
        .await
        .unwrap();
    route_update::run(&ctx, &fx.site_id).await.expect("route");
    let last = agent.commands_of("set_route").pop().unwrap();
    assert_eq!(last["basic_auth"]["username"], "alice");
    assert_eq!(last["basic_auth"]["password_hash"], hash.as_str());
    assert_eq!(
        *logs.lock().unwrap(),
        ["step set_route: ok", "step finish: ok"].map(str::to_string)
    );

    assert!(fx.db.basic_auths().clear(&fx.site_id).await.unwrap());
    route_update::run(&ctx, &fx.site_id).await.expect("route");
    assert!(agent.commands_of("set_route").pop().unwrap()["basic_auth"].is_null());
}

#[tokio::test]
async fn an_agent_failure_fails_the_job_and_keeps_the_site_status() {
    let (_test_db, fx) = setup(&manifest_toml("0.1.0")).await;
    let agent = FakeAgent::new();
    let (ctx, _logs) = ctx_with_logs(fx.db.clone(), &agent);
    provision::run(&ctx, &fx.site_id).await.expect("provision");
    agent.fail_next("set_route");
    route_update::run(&ctx, &fx.site_id)
        .await
        .expect_err("agent failed");
    assert_eq!(
        fx.db.sites().get(&fx.site_id).await.unwrap().status,
        "running"
    );
}

#[tokio::test]
async fn a_site_that_is_not_settled_is_refused() {
    let (_test_db, fx) = setup(&manifest_toml("0.1.0")).await;
    let agent = FakeAgent::new();
    let (ctx, _logs) = ctx_with_logs(fx.db.clone(), &agent);
    let err = route_update::run(&ctx, &fx.site_id)
        .await
        .expect_err("still provisioning");
    assert!(err.message.contains("cannot change its route"), "{err}");
}
