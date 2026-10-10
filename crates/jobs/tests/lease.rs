//! A run that lost its job lease stops: no further writes, every later
//! step fails at once with `conflict` (M2).

mod common;

use common::{exec, job_ctx, manifest_toml, setup};
use setu_common::ErrorCode;
use setu_jobs::testing::FakeAgent;

#[tokio::test]
async fn lost_lease_stops_the_run() {
    let (test_db, fx) = setup(&manifest_toml("0.1.0")).await;
    let agent = FakeAgent::new();
    let (ctx, job_id, _logs) = job_ctx(
        &fx.db,
        &agent,
        "backup_site",
        &fx.site_id,
        serde_json::json!({}),
    )
    .await;

    ctx.step("one", async { Ok(()) }).await.expect("held: ok");
    // Another runner reclaims the job.
    exec(
        &test_db,
        "UPDATE jobs SET locked_by = 'other-runner' WHERE id = $1",
        &job_id,
    )
    .await;
    let err = ctx
        .step("two", async { Ok(()) })
        .await
        .expect_err("set_step refused");
    assert_eq!(err.code, ErrorCode::Conflict);
    assert!(ctx.lease_lost());
    let mut ran = false;
    let err = ctx
        .step("three", async {
            ran = true;
            Ok(())
        })
        .await
        .expect_err("fatal for the run");
    assert_eq!(err.code, ErrorCode::Conflict);
    assert!(!ran, "no step body runs after the lease is lost");
    let job = fx.db.jobs().get(&job_id).await.unwrap();
    assert_eq!(job.payload["step"], "one");
    assert_eq!(job.log, "step one: ok\n");
}
