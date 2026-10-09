//! Provision state-machine tests over [`FakeAgent`].

mod common;

use common::{ctx_with_logs, manifest_toml, manifest_toml_image, setup, test_sealer};
use setu_jobs::{provision, testing::FakeAgent, JobCtx};

fn kinds(transcript: &[String]) -> Vec<&str> {
    transcript.iter().map(String::as_str).collect()
}

#[tokio::test]
async fn provision_happy_path_issues_commands_in_order() {
    let (_test_db, fx) = setup(&manifest_toml("0.1.0")).await;
    let agent = FakeAgent::new();
    let (ctx, logs) = ctx_with_logs(fx.db.clone(), &agent);

    provision::run(&ctx, &fx.site_id)
        .await
        .expect("provision must succeed");

    // Health, then SetRoute, then one more Status after routing; the
    // second WriteEnv drops `once` secrets from the agent env file.
    assert_eq!(
        kinds(&agent.transcript()),
        [
            "create_database",
            "install",
            "write_unit",
            "write_env",
            "start",
            "health",
            "set_route",
            "write_env",
            "status",
        ]
    );

    let site = fx.db.sites().get(&fx.site_id).await.unwrap();
    assert_eq!(site.status, "running");
    assert_eq!(site.failed_step, None);

    let writes = agent.write_env_calls();
    assert_eq!(writes.len(), 2, "full env then env without once vars");
    assert!(
        writes[0].iter().any(|v| v.once),
        "first WriteEnv carries once secrets"
    );
    assert!(
        writes[1].iter().all(|v| !v.once),
        "second WriteEnv drops once secrets"
    );
    assert!(writes[0].len() > writes[1].len());

    // Sealed copy is stored: secrets carry the `v1:` prefix, flags kept.
    let stored = site.env;
    let vars = stored.as_array().expect("env stored as array");
    assert_eq!(vars.len(), writes[0].len());
    for var in vars {
        if var["secret"].as_bool().unwrap_or(false) {
            assert!(
                var["value"].as_str().unwrap_or("").starts_with("v1:"),
                "secret must be sealed"
            );
        }
    }

    let lines = logs.lock().expect("log lock");
    assert!(lines.iter().any(|l| l == "step validate: ok"));
    assert!(lines.iter().any(|l| l == "step health: ok"));
    assert!(lines.iter().any(|l| l == "step finish: ok"));
    for line in lines.iter() {
        assert!(line.starts_with("step "), "unexpected log line: {line}");
        assert!(!line.contains("v1:"), "sealed value leaked: {line}");
    }
    // Generated plaintext env values (captured from the WriteEnv
    // payloads) must never appear in any log line.
    let plaintext: Vec<String> = writes
        .iter()
        .flat_map(|env| env.iter().map(|v| v.value.clone()))
        .filter(|v| !v.is_empty())
        .collect();
    assert!(!plaintext.is_empty(), "expected generated env values");
    for value in &plaintext {
        for line in lines.iter() {
            assert!(
                !line.contains(value.as_str()),
                "plaintext env value leaked: {line}"
            );
        }
    }

    // The context redacts its sealer.
    let debug = format!("{ctx:?}");
    assert!(debug.contains("[redacted]"));
    let _ = (test_sealer(), std::mem::size_of::<JobCtx>());
}

#[tokio::test]
async fn provision_failure_at_install_marks_failed_with_step() {
    let (_test_db, fx) = setup(&manifest_toml("0.1.0")).await;
    let agent = FakeAgent::new();
    agent.fail_at("install", 1);
    let (ctx, logs) = ctx_with_logs(fx.db.clone(), &agent);

    let _err = provision::run(&ctx, &fx.site_id)
        .await
        .expect_err("must fail");

    let site = fx.db.sites().get(&fx.site_id).await.unwrap();
    assert_eq!(site.status, "failed");
    assert_eq!(site.failed_step.as_deref(), Some("install"));
    assert_eq!(kinds(&agent.transcript()), ["create_database", "install"]);

    let lines = logs.lock().expect("log lock");
    assert!(
        lines
            .iter()
            .any(|l| l == "step install: error injected failure"),
        "lines: {lines:?}"
    );
}

#[tokio::test]
async fn crash_after_health_before_route_is_failed() {
    let (_test_db, fx) = setup(&manifest_toml("0.1.0")).await;
    let agent = FakeAgent::new();
    agent.set_status_active(false);
    let (ctx, _logs) = ctx_with_logs(fx.db.clone(), &agent);

    let _err = provision::run(&ctx, &fx.site_id)
        .await
        .expect_err("must fail");

    let site = fx.db.sites().get(&fx.site_id).await.unwrap();
    assert_eq!(site.status, "failed");
    assert_eq!(site.failed_step.as_deref(), Some("verify_status"));

    // Health passed and the route was set, but the post-route Status
    // found a dead site: the route is removed again.
    let transcript = agent.transcript();
    assert_eq!(
        kinds(&transcript),
        [
            "create_database",
            "install",
            "write_unit",
            "write_env",
            "start",
            "health",
            "set_route",
            "write_env",
            "status",
            "remove_route",
        ]
    );
}

#[tokio::test]
async fn redelivered_command_is_idempotent() {
    let (_test_db, fx) = setup(&manifest_toml("0.1.0")).await;
    let agent = FakeAgent::new();
    // The first WriteUnit delivery times out (agent restarted
    // mid-command); the retry succeeds.
    agent.timeout_once("write_unit");
    let (ctx, _logs) = ctx_with_logs(fx.db.clone(), &agent);

    provision::run(&ctx, &fx.site_id)
        .await
        .expect("retry must succeed");

    let transcript = agent.transcript();
    let write_units = transcript
        .iter()
        .filter(|k| k.as_str() == "write_unit")
        .count();
    assert_eq!(write_units, 2, "transcript: {transcript:?}");
    let site = fx.db.sites().get(&fx.site_id).await.unwrap();
    assert_eq!(site.status, "running");
}

#[tokio::test]
async fn retry_from_failed_reruns_all_steps() {
    let (_test_db, fx) = setup(&manifest_toml("0.1.0")).await;
    let agent = FakeAgent::new();
    agent.fail_at("install", 1);
    let (ctx, _logs) = ctx_with_logs(fx.db.clone(), &agent);

    provision::run(&ctx, &fx.site_id)
        .await
        .expect_err("first run fails");
    let site = fx.db.sites().get(&fx.site_id).await.unwrap();
    assert_eq!(site.status, "failed");

    // The recorded failure was for the first Install call only; the
    // retry re-runs every step from scratch and succeeds.
    provision::run(&ctx, &fx.site_id)
        .await
        .expect("retry must succeed");
    let site = fx.db.sites().get(&fx.site_id).await.unwrap();
    assert_eq!(site.status, "running");
    assert_eq!(site.failed_step, None);

    let transcript = agent.transcript();
    assert_eq!(transcript.len(), 2 + 9, "transcript: {transcript:?}");
    assert_eq!(agent.call_count("install"), 2);
    assert_eq!(
        kinds(&transcript[2..]),
        [
            "create_database",
            "install",
            "write_unit",
            "write_env",
            "start",
            "health",
            "set_route",
            "write_env",
            "status",
        ]
    );
}

#[tokio::test]
async fn provision_rejects_image_manifest_before_any_agent_call() {
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
async fn second_write_env_failure_removes_route_and_marks_failed() {
    let (_test_db, fx) = setup(&manifest_toml("0.1.0")).await;
    let agent = FakeAgent::new();
    // Only the second WriteEnv (once-clearing) fails; the first succeeds.
    agent.fail_at("write_env", 2);
    let (ctx, _logs) = ctx_with_logs(fx.db.clone(), &agent);

    let _err = provision::run(&ctx, &fx.site_id)
        .await
        .expect_err("must fail");

    let site = fx.db.sites().get(&fx.site_id).await.unwrap();
    assert_eq!(site.status, "failed");
    assert_eq!(site.failed_step.as_deref(), Some("set_route"));

    // SetRoute succeeded, so the route must be removed again after the
    // once-clearing WriteEnv failed.
    let transcript = agent.transcript();
    let set_pos = transcript
        .iter()
        .position(|k| k == "set_route")
        .expect("set_route issued");
    let remove_pos = transcript
        .iter()
        .position(|k| k == "remove_route")
        .expect("remove_route issued");
    assert!(
        remove_pos > set_pos,
        "RemoveRoute must follow SetRoute: {transcript:?}"
    );
}
