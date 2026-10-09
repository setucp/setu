//! Backup and restore over [`FakeAgent`]: kinds, TTLs, ids.

mod common;

use common::{ctx_with_logs, manifest_toml, setup};
use setu_jobs::{backup, provision, restore, testing::FakeAgent};

#[tokio::test]
async fn backup_uses_60_minute_ttl() {
    let (_test_db, fx) = setup(&manifest_toml("0.1.0")).await;
    let agent = FakeAgent::new();
    let (ctx, _logs) = ctx_with_logs(fx.db.clone(), &agent);
    provision::run(&ctx, &fx.site_id).await.expect("provision");

    backup::run(&ctx, &fx.site_id, "manual")
        .await
        .expect("backup");
    let rows = fx.db.backups().list_for_site(&fx.site_id).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0].kind.as_str(), rows[0].status.as_str()),
        ("manual", "completed")
    );

    restore::run(&ctx, &fx.site_id, &rows[0].id)
        .await
        .expect("restore");
    let ttls: Vec<(String, u64)> = agent
        .ttls()
        .into_iter()
        .filter(|(k, _)| k == "backup" || k == "restore")
        .map(|(k, d)| (k, d.as_secs()))
        .collect();
    assert_eq!(
        ttls,
        [("backup".to_string(), 3600), ("restore".to_string(), 3600)]
    );
    backup::run(&ctx, &fx.site_id, "pre_update")
        .await
        .expect_err("pre_* backups are taken by their flows, not as jobs");
}
