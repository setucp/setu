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
    let (_, job) = fx
        .db
        .basic_auths()
        .set_with_route_job(
            &fx.site_id,
            &fx.server_id,
            "alice",
            &hash,
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    assert_eq!(job.kind, "route_update");
    route_update::run(&ctx, &fx.site_id).await.expect("route");
    finish(&fx.db, &job.id).await;
    let last = agent.commands_of("set_route").pop().unwrap();
    assert_eq!(last["basic_auth"]["username"], "alice");
    assert_eq!(last["basic_auth"]["password_hash"], hash.as_str());
    assert_eq!(
        *logs.lock().unwrap(),
        ["step set_route: ok", "step finish: ok"].map(str::to_string)
    );

    let job = fx
        .db
        .basic_auths()
        .clear_with_route_job(&fx.site_id, &fx.server_id)
        .await
        .unwrap()
        .expect("had credentials");
    route_update::run(&ctx, &fx.site_id).await.expect("route");
    assert!(agent.commands_of("set_route").pop().unwrap()["basic_auth"].is_null());
    finish(&fx.db, &job.id).await;
    assert!(fx
        .db
        .basic_auths()
        .clear_with_route_job(&fx.site_id, &fx.server_id)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn credentials_stay_unchanged_when_the_route_job_cannot_be_queued() {
    let (_test_db, fx) = setup(&manifest_toml("0.1.0")).await;
    let hash = format!("$2b$12${}", "a".repeat(53));
    let ba = fx.db.basic_auths();
    let (_, first) = ba
        .set_with_route_job(
            &fx.site_id,
            &fx.server_id,
            "alice",
            &hash,
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    // `first` is still queued: the next change cannot queue its job.
    let other = format!("$2b$12${}", "b".repeat(53));
    let err = ba
        .set_with_route_job(
            &fx.site_id,
            &fx.server_id,
            "bob",
            &other,
            chrono::Utc::now(),
        )
        .await
        .expect_err("active job");
    assert_eq!(err.code, setu_common::ErrorCode::Conflict, "{err}");
    assert_eq!(
        ba.get(&fx.site_id).await.unwrap().unwrap().username,
        "alice"
    );
    ba.clear_with_route_job(&fx.site_id, &fx.server_id)
        .await
        .expect_err("active job");
    assert!(ba.get(&fx.site_id).await.unwrap().is_some());
    let latest = fx
        .db
        .jobs()
        .latest_for_site(&fx.site_id, "route_update")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(latest.id, first.id);
}

/// Claim the queued job and mark it done, as the runner would.
async fn finish(db: &setu_db::Db, id: &str) {
    let claimed = db
        .jobs()
        .claim("test-worker", std::time::Duration::from_secs(60))
        .await
        .unwrap()
        .expect("a queued job");
    assert_eq!(claimed.id, id);
    db.jobs()
        .finish(id, "test-worker", true, None)
        .await
        .unwrap();
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
