//! Runner tests: leases renewed while a job runs, a killed runner's job
//! reclaimed by another, and at most two installs per server.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{add_site, manifest_toml, setup, test_sealer, wait_job};
use setu_jobs::{testing::FakeAgent, Runner, RunnerConfig, SystemClock};

fn fast(concurrency: usize) -> RunnerConfig {
    RunnerConfig {
        concurrency,
        lease: Duration::from_secs(1),
        renew_every: Duration::from_millis(200),
        installs_per_server: 2,
        idle_poll: Duration::from_millis(50),
        verify_delay: Duration::ZERO,
    }
}

fn spawn(db: &setu_db::Db, agent: &FakeAgent, config: RunnerConfig) -> tokio::task::JoinHandle<()> {
    Runner::spawn(
        db.clone(),
        Arc::new(agent.clone()),
        test_sealer(),
        Arc::new(SystemClock),
        config,
    )
}

#[tokio::test]
async fn job_lease_is_renewed_while_running() {
    let (_test_db, fx) = setup(&manifest_toml("0.1.0")).await;
    let agent = FakeAgent::new();
    // The install outlives the 1 s lease three times over.
    agent.delay("install", Duration::from_secs(3));
    let job = fx
        .db
        .jobs()
        .enqueue(
            "provision_site",
            Some(&fx.site_id),
            Some(&fx.server_id),
            None,
        )
        .await
        .unwrap();
    let first = spawn(&fx.db, &agent, fast(1));
    // A second runner polls the whole time: it would reclaim the job
    // the moment the lease lapsed.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let second = spawn(&fx.db, &agent, fast(1));

    tokio::time::sleep(Duration::from_millis(2200)).await;
    let mid = fx.db.jobs().get(&job.id).await.unwrap();
    assert_eq!(mid.status, "running");
    assert_eq!(mid.attempts, 1, "lease lapsed and the job was reclaimed");
    let until = mid.locked_until.expect("leased");
    assert!(until > chrono::Utc::now(), "lease not renewed: {until}");

    let done = wait_job(&fx.db, &job.id, Duration::from_secs(10)).await;
    assert_eq!(done.status, "done", "error: {:?}", done.error);
    assert_eq!(done.attempts, 1);
    assert_eq!(agent.call_count("install"), 1);
    first.abort();
    second.abort();
}

#[tokio::test]
async fn killed_runner_job_is_reclaimed_and_finishes() {
    let (_test_db, fx) = setup(&manifest_toml("0.1.0")).await;
    let agent = FakeAgent::new();
    agent.delay("install", Duration::from_millis(1500));
    let job = fx
        .db
        .jobs()
        .enqueue(
            "provision_site",
            Some(&fx.site_id),
            Some(&fx.server_id),
            None,
        )
        .await
        .unwrap();
    let first = spawn(&fx.db, &agent, fast(1));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while agent.call_count("install") == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "install never issued"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Kill the runner mid-install: its workers and renewal die with it.
    first.abort();
    let _ = first.await;
    assert_eq!(agent.max_concurrent("install"), 1);

    let second = spawn(&fx.db, &agent, fast(1));
    let done = wait_job(&fx.db, &job.id, Duration::from_secs(15)).await;
    assert_eq!(done.status, "done", "error: {:?}", done.error);
    assert_eq!(
        done.attempts, 2,
        "claimed by the first, reclaimed by the second"
    );
    let installs = agent
        .transcript()
        .iter()
        .filter(|k| k.as_str() == "install")
        .count();
    assert_eq!(installs, 2, "transcript: {:?}", agent.transcript());
    // Resumed from the cursor (`install`): the earlier steps ran once.
    assert_eq!(
        agent.call_count("site_create"),
        1,
        "{:?}",
        agent.transcript()
    );
    assert_eq!(agent.call_count("create_database"), 1);
    let site = fx.db.sites().get(&fx.site_id).await.unwrap();
    assert_eq!(site.status, "running");
    second.abort();
}

#[tokio::test]
async fn at_most_two_installs_per_server() {
    let (_test_db, fx) = setup(&manifest_toml("0.1.0")).await;
    let mut sites = vec![fx.site_id.clone()];
    for i in 1..4 {
        sites.push(add_site(&fx.db, &fx.site_id, &format!("h{i}.example.com")).await);
    }
    let agent = FakeAgent::new();
    agent.delay("install", Duration::from_secs(1));
    let mut jobs = Vec::new();
    for site in &sites {
        let job = fx
            .db
            .jobs()
            .enqueue("provision_site", Some(site), Some(&fx.server_id), None)
            .await
            .unwrap();
        jobs.push(job.id);
    }
    let runner = spawn(
        &fx.db,
        &agent,
        RunnerConfig {
            lease: Duration::from_secs(10),
            ..fast(4)
        },
    );
    for job in &jobs {
        let done = wait_job(&fx.db, job, Duration::from_secs(20)).await;
        assert_eq!(done.status, "done", "error: {:?}", done.error);
    }
    assert_eq!(agent.call_count("install"), 4);
    assert_eq!(
        agent.max_concurrent("install"),
        2,
        "two install slots per server"
    );
    // Other steps are not throttled: the four jobs did run concurrently.
    assert!(agent.max_concurrent("site_create") >= 1);
    runner.abort();
}
