// Test crate: a panic is the failure report (R146).
#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Provision state-machine tests over [`FakeAgent`] (plan Task 6).

mod common;

use common::{
    ctx_with_logs, finish_job, job_ctx, manifest_toml, manifest_toml_image, setup, stored_env,
    url_password,
};
use setu_jobs::{provision, testing::FakeAgent};

fn kinds(transcript: &[String]) -> Vec<&str> {
    transcript.iter().map(String::as_str).collect()
}

/// `inventory` is the `validate` step's port check; then the eleven
/// agent steps of spec §6 (`status` is `verify`, the last `write_env`
/// is `finish`).
const HAPPY: [&str; 12] = [
    "inventory",
    "site_create",
    "create_database",
    "install",
    "switch_version",
    "write_unit",
    "write_env",
    "start",
    "health",
    "set_route",
    "status",
    "write_env",
];

#[tokio::test]
#[allow(clippy::format_collect, clippy::match_same_arms)] // reviewed: kept as-is (R150)
async fn provision_happy_path_transcript() {
    let (_test_db, fx) = setup(&manifest_toml("0.1.0")).await;
    let agent = FakeAgent::new();
    let (ctx, job_id, logs) = job_ctx(
        &fx.db,
        &agent,
        "provision_site",
        &fx.site_id,
        serde_json::json!({}),
    )
    .await;

    provision::run(&ctx, &fx.site_id)
        .await
        .expect("provision must succeed");

    assert_eq!(kinds(&agent.transcript()), HAPPY);
    let site = fx.db.sites().get(&fx.site_id).await.unwrap();
    assert_eq!(site.status, "running");
    assert_eq!(site.failed_step, None);

    // First boot carries the `once` value; `finish` drops it.
    let writes = agent.write_env_calls();
    assert_eq!(writes.len(), 2);
    assert!(writes[0]
        .iter()
        .any(|v| v.once && v.name == "HELLO_PASSWORD"));
    assert!(writes[1]
        .iter()
        .all(|v| !v.once && v.name != "HELLO_PASSWORD"));
    assert_eq!(writes[0].len(), writes[1].len() + 1);

    // The written env IS the env stored at creation, never regenerated.
    let stored = stored_env(&site);
    for var in &writes[0] {
        assert_eq!(stored.get(&var.name), Some(&var.value), "{}", var.name);
    }

    // The database password travels only where it is needed.
    let password = url_password(&stored["HELLO_DATABASE_URL"]);
    assert_eq!(
        agent.saw_secret(&password),
        ["create_database", "write_env", "write_env"]
    );
    let job = fx.db.jobs().get(&job_id).await.unwrap();
    assert!(!job.log.contains(&password), "password in jobs.log");
    for value in stored.values().filter(|v| v.len() >= 8) {
        assert!(!job.log.contains(value.as_str()), "env value in jobs.log");
    }
    // Every step logged, in order, and the cursor is at the end.
    let expect: Vec<String> = provision::STEPS
        .iter()
        .map(|s| format!("step {s}: ok"))
        .collect();
    assert_eq!(*logs.lock().unwrap(), expect);
    assert_eq!(
        job.log,
        expect.iter().map(|l| format!("{l}\n")).collect::<String>()
    );
    assert_eq!(job.payload["step"], "finish");
    assert!(job.payload.get("prompts").is_none());
    // Every TTL follows the kind.
    for (kind, ttl) in agent.ttls() {
        let want = match kind.as_str() {
            "install" => 15 * 60,
            "health" => 90 + 30,
            _ => 2 * 60,
        };
        assert_eq!(ttl.as_secs(), want, "{kind}");
    }
    assert!(format!("{ctx:?}").contains("[redacted]"));
}

#[tokio::test]
async fn provision_failure_marks_failed_with_step() {
    let (_test_db, fx) = setup(&manifest_toml("0.1.0")).await;
    let agent = FakeAgent::new();
    agent.fail_at("install", 1);
    let (ctx, logs) = ctx_with_logs(fx.db.clone(), &agent);

    provision::run(&ctx, &fx.site_id)
        .await
        .expect_err("must fail");

    let site = fx.db.sites().get(&fx.site_id).await.unwrap();
    assert_eq!(site.status, "failed");
    assert_eq!(site.failed_step.as_deref(), Some("install"));
    assert_eq!(
        kinds(&agent.transcript()),
        ["inventory", "site_create", "create_database", "install"]
    );
    assert_eq!(
        logs.lock().unwrap().last().map(String::as_str),
        Some("step install: error injected failure")
    );

    // An agent error that echoes a secret is redacted in the log line,
    // in `jobs.log` and in the returned error.
    let site = fx.db.sites().get(&fx.site_id).await.unwrap();
    let password = url_password(&stored_env(&site)["HELLO_DATABASE_URL"]);
    agent.fail_with("start", 1, &format!("auth failed for {password}"));
    let (ctx, job_id, logs) = job_ctx(
        &fx.db,
        &agent,
        "provision_site",
        &fx.site_id,
        serde_json::json!({}),
    )
    .await;
    let err = provision::run(&ctx, &fx.site_id)
        .await
        .expect_err("start fails");
    assert_eq!(err.message, "auth failed for ***");
    assert_eq!(
        logs.lock().unwrap().last().map(String::as_str),
        Some("step start: error auth failed for ***")
    );
    let job = fx.db.jobs().get(&job_id).await.unwrap();
    assert!(!job.log.contains(&password));
    assert_eq!(
        fx.db
            .sites()
            .get(&fx.site_id)
            .await
            .unwrap()
            .failed_step
            .as_deref(),
        Some("start")
    );
}

#[tokio::test]
async fn crash_after_health_before_route_is_failed() {
    let (_test_db, fx) = setup(&manifest_toml("0.1.0")).await;
    let agent = FakeAgent::new();
    // The app passed health, then died: `verify` finds it inactive.
    agent.status_inactive_after(0);
    let (ctx, _logs) = ctx_with_logs(fx.db.clone(), &agent);

    provision::run(&ctx, &fx.site_id)
        .await
        .expect_err("must fail");

    let site = fx.db.sites().get(&fx.site_id).await.unwrap();
    assert_eq!(site.status, "failed");
    assert_eq!(site.failed_step.as_deref(), Some("verify"));
    // The route is removed again and `finish` never runs.
    let mut expect = HAPPY[..11].to_vec();
    expect.push("remove_route");
    assert_eq!(kinds(&agent.transcript()), expect);
}

#[tokio::test]
async fn timed_out_step_is_retried_once_with_a_new_command() {
    let (_test_db, fx) = setup(&manifest_toml("0.1.0")).await;
    let agent = FakeAgent::new();
    agent.timeout_at("write_unit", 1);
    let (ctx, _logs) = ctx_with_logs(fx.db.clone(), &agent);

    provision::run(&ctx, &fx.site_id)
        .await
        .expect("one retry recovers");
    assert_eq!(agent.call_count("write_unit"), 2);
    assert_eq!(
        fx.db.sites().get(&fx.site_id).await.unwrap().status,
        "running"
    );

    // Two timeouts in a row: one retry only, then the step fails.
    let (_test_db2, fx2) = setup(&manifest_toml("0.1.0")).await;
    let agent2 = FakeAgent::new();
    agent2.timeout_at("start", 1);
    agent2.timeout_at("start", 2);
    let (ctx2, logs2) = ctx_with_logs(fx2.db.clone(), &agent2);
    provision::run(&ctx2, &fx2.site_id)
        .await
        .expect_err("second timeout fails the step");
    assert_eq!(agent2.call_count("start"), 2);
    assert_eq!(agent2.call_count("health"), 0);
    let site = fx2.db.sites().get(&fx2.site_id).await.unwrap();
    assert_eq!(
        (site.status.as_str(), site.failed_step.as_deref()),
        ("failed", Some("start"))
    );
    assert!(logs2
        .lock()
        .unwrap()
        .iter()
        .any(|l| l.starts_with("step start: error agent timed out")));
}

#[tokio::test]
async fn retry_resumes_with_same_allocations() {
    let (_test_db, fx) = setup(&manifest_toml("0.1.0")).await;
    let created = fx.db.sites().get(&fx.site_id).await.unwrap();
    let agent = FakeAgent::new();
    agent.fail_at("start", 1);
    let (ctx, job_id, _) = job_ctx(
        &fx.db,
        &agent,
        "provision_site",
        &fx.site_id,
        serde_json::json!({}),
    )
    .await;
    provision::run(&ctx, &fx.site_id)
        .await
        .expect_err("first run fails at start");
    finish_job(&fx.db, &job_id, false).await;
    let first = agent.transcript().len();

    // The retry is a new job: from step 1, with the same allocations.
    let (ctx, _job_id, _) = job_ctx(
        &fx.db,
        &agent,
        "provision_site",
        &fx.site_id,
        serde_json::json!({}),
    )
    .await;
    provision::run(&ctx, &fx.site_id)
        .await
        .expect("retry succeeds");
    assert_eq!(kinds(&agent.transcript()[first..]), HAPPY);

    let site = fx.db.sites().get(&fx.site_id).await.unwrap();
    assert_eq!(site.status, "running");
    assert_eq!(
        (site.port, &site.short_id, &site.db_name),
        (created.port, &created.short_id, &created.db_name)
    );
    assert_eq!(stored_env(&site), stored_env(&created), "env regenerated");
    let passwords: Vec<String> = agent
        .commands_of("create_database")
        .iter()
        .map(|c| c["password"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(passwords.len(), 2);
    assert_eq!(passwords[0], passwords[1], "db password changed on retry");
    let writes = agent.write_env_calls();
    assert_eq!(writes[0], writes[1], "first-boot env changed on retry");
}

#[tokio::test]
async fn provision_rejects_image_manifest_in_validate() {
    let (_test_db, fx) = setup(&manifest_toml_image("0.3.0")).await;
    let agent = FakeAgent::new();
    let (ctx, _logs) = ctx_with_logs(fx.db.clone(), &agent);

    let err = provision::run(&ctx, &fx.site_id)
        .await
        .expect_err("must fail");
    assert!(
        err.message.contains("image runtime not available"),
        "unexpected error: {err}"
    );
    assert!(agent.transcript().is_empty(), "no command may be issued");
    let site = fx.db.sites().get(&fx.site_id).await.unwrap();
    assert_eq!(site.status, "failed");
    assert_eq!(site.failed_step.as_deref(), Some("validate"));
}

#[tokio::test]
async fn provision_validate_checks_port_free() {
    let (_test_db, fx) = setup(&manifest_toml("0.1.0")).await;
    let site = fx.db.sites().get(&fx.site_id).await.unwrap();
    let agent = FakeAgent::new();
    agent.set_listening_ports(vec![22, u16::try_from(site.port).unwrap()]);
    let (ctx, _logs) = ctx_with_logs(fx.db.clone(), &agent);

    let err = provision::run(&ctx, &fx.site_id)
        .await
        .expect_err("busy port must fail");
    assert!(err.message.contains("already in use"), "{err}");
    assert_eq!(kinds(&agent.transcript()), ["inventory"]);
    let site = fx.db.sites().get(&fx.site_id).await.unwrap();
    assert_eq!(site.status, "failed");
    assert_eq!(site.failed_step.as_deref(), Some("validate"));

    // Another port busy: fine.
    agent.set_listening_ports(vec![22, 20999]);
    provision::run(&ctx, &fx.site_id)
        .await
        .expect("free port provisions");
}
