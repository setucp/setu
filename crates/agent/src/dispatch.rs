//! Command dispatch: every `Command` variant handled against a
//! `NativeRuntime`. Handlers are idempotent (check state, then act) so the
//! control plane may redeliver freely after timeouts.
//!
//! Result logs never carry env values or passwords.

use setu_proto::{Command, CommandResult, InventoryReport, LogsOutput, UnitState};

use crate::caddy::CaddyAdmin;
use crate::helper_client::Helper;
use crate::postgres::PostgresAdmin;
use crate::runtime::{NativeRuntime, RuntimeStatus};

fn ok(id: &str, output: serde_json::Value, log: &str) -> CommandResult {
    CommandResult {
        id: id.to_string(),
        ok: true,
        output,
        log: log.to_string(),
        error: None,
    }
}

fn err(id: &str, log: &str, e: impl std::fmt::Display) -> CommandResult {
    // One line on the wire; detail stays in the agent journal.
    let msg = e.to_string();
    let first = msg.lines().next().unwrap_or("command failed");
    CommandResult {
        id: id.to_string(),
        ok: false,
        output: serde_json::Value::Null,
        log: log.to_string(),
        error: Some(first.to_string()),
    }
}

/// Handle one command to a result. Covers every `Command` variant; there
/// is no fallthrough arm, so adding a variant breaks compilation here.
pub async fn handle<H, P, C>(
    cmd_id: &str,
    cmd: &Command,
    rt: &NativeRuntime<H, P, C>,
) -> CommandResult
where
    H: Helper,
    P: PostgresAdmin + Send + Sync,
    C: CaddyAdmin,
{
    match cmd {
        Command::SiteCreate { site } => match rt.site_create(site).await {
            Ok(()) => ok(cmd_id, serde_json::json!({}), "site_create ok"),
            Err(e) => err(cmd_id, "site_create failed", e),
        },
        Command::CreateDatabase { db, role, password } => {
            match rt.postgres().create_or_alter(db, role, password).await {
                Ok(()) => ok(cmd_id, serde_json::json!({"db": db}), "create_database ok"),
                Err(e) => err(cmd_id, "create_database failed", e),
            }
        }
        Command::DropDatabase { db, role } => match rt.postgres().drop(db, role).await {
            Ok(()) => ok(cmd_id, serde_json::json!({"db": db}), "drop_database ok"),
            Err(e) => err(cmd_id, "drop_database failed", e),
        },
        Command::Install { site, binary } => match rt.install_version(site, binary).await {
            Ok(version) => ok(
                cmd_id,
                serde_json::json!({"version": version}),
                "install ok",
            ),
            Err(e) => err(cmd_id, "install failed", e),
        },
        Command::SwitchVersion { site, version } => {
            match rt.switch_version_inner(site, version).await {
                Ok(changed) => ok(
                    cmd_id,
                    serde_json::json!({"version": version, "changed": changed}),
                    "switch_version ok",
                ),
                Err(e) => err(cmd_id, "switch_version failed", e),
            }
        }
        Command::WriteUnit { site } => match rt.write_unit_inner(site).await {
            Ok(changed) => ok(
                cmd_id,
                serde_json::json!({"changed": changed}),
                "write_unit ok",
            ),
            Err(e) => err(cmd_id, "write_unit failed", e),
        },
        Command::WriteEnv { site, env } => match rt.write_env_inner(site, env).await {
            Ok(()) => ok(
                cmd_id,
                serde_json::json!({"vars": env.len()}),
                &format!("write_env ok ({} vars)", env.len()),
            ),
            Err(e) => err(cmd_id, "write_env failed", e),
        },
        Command::Start { site } => match rt.start_inner(site).await {
            Ok(()) => ok(cmd_id, serde_json::json!({}), "start ok"),
            Err(e) => err(cmd_id, "start failed", e),
        },
        Command::Stop { site } => match rt.stop_inner(site).await {
            Ok(()) => ok(cmd_id, serde_json::json!({}), "stop ok"),
            Err(e) => err(cmd_id, "stop failed", e),
        },
        Command::Restart { site } => match rt.restart_inner(site).await {
            Ok(()) => ok(cmd_id, serde_json::json!({}), "restart ok"),
            Err(e) => err(cmd_id, "restart failed", e),
        },
        Command::Status { site } => match rt.status_inner(site).await {
            Ok(st) => ok(
                cmd_id,
                serde_json::json!({
                    "active": st.active,
                    "sub": st.sub,
                    "pid": st.pid,
                    "n_restarts": st.n_restarts,
                    "data_bytes": st.data_bytes,
                }),
                "status ok",
            ),
            Err(e) => err(cmd_id, "status failed", e),
        },
        Command::Health { site } => match rt.health_inner(site).await {
            Ok(status) => ok(cmd_id, serde_json::json!({"http": status}), "health ok"),
            Err(e) => err(cmd_id, "health failed", e),
        },
        Command::SetRoute {
            site_short_id,
            domain,
            port,
        } => {
            let id = match crate::caddy::site_route_id(site_short_id) {
                Ok(id) => id,
                Err(e) => return err(cmd_id, "set_route failed", e),
            };
            let upstream = crate::caddy::site_upstream(*port);
            match rt.caddy().upsert_route(&id, domain, &upstream).await {
                Ok(()) => ok(
                    cmd_id,
                    serde_json::json!({"domain": domain, "route_id": id}),
                    "set_route ok",
                ),
                Err(e) => err(cmd_id, "set_route failed", e),
            }
        }
        Command::RemoveRoute {
            site_short_id,
            domain,
        } => {
            let id = match crate::caddy::site_route_id(site_short_id) {
                Ok(id) => id,
                Err(e) => return err(cmd_id, "remove_route failed", e),
            };
            match rt.caddy().remove_route(&id).await {
                Ok(()) => ok(
                    cmd_id,
                    serde_json::json!({"domain": domain, "route_id": id}),
                    "remove_route ok",
                ),
                Err(e) => err(cmd_id, "remove_route failed", e),
            }
        }
        Command::Backup {
            site,
            db,
            dirs,
            backup_id,
        } => match crate::backup::backup_site(rt, site, db, dirs, backup_id).await {
            Ok((_, size)) => ok(
                cmd_id,
                serde_json::json!({"backup_id": backup_id, "size_bytes": size}),
                "backup ok",
            ),
            Err(e) => err(cmd_id, "backup failed", e),
        },
        Command::Restore {
            site,
            db,
            role,
            dirs,
            backup_id,
        } => match crate::backup::restore_site(rt, site, db, role, dirs, backup_id).await {
            Ok(()) => ok(
                cmd_id,
                serde_json::json!({"backup_id": backup_id}),
                "restore ok",
            ),
            Err(e) => err(cmd_id, "restore failed", e),
        },
        Command::DeleteBackup { site, backup_id } => {
            match crate::backup::delete_backup(&rt.config().backups_root, site, backup_id) {
                Ok(existed) => ok(
                    cmd_id,
                    serde_json::json!({"backup_id": backup_id, "existed": existed}),
                    "delete_backup ok",
                ),
                Err(e) => err(cmd_id, "delete_backup failed", e),
            }
        }
        Command::SiteRemove { site } => match rt.remove_inner(site).await {
            Ok(()) => ok(cmd_id, serde_json::json!({}), "site_remove ok"),
            Err(e) => err(cmd_id, "site_remove failed", e),
        },
        Command::FetchLogs {
            site,
            cursor,
            lines,
        } => match rt
            .logs_inner(site, cursor.as_deref(), (*lines).clamp(1, 5000))
            .await
        {
            Ok(stream) => ok(
                cmd_id,
                serde_json::to_value(LogsOutput {
                    lines: stream.lines,
                    cursor: stream.next,
                })
                .unwrap_or_default(),
                "fetch_logs ok",
            ),
            Err(e) => err(cmd_id, "fetch_logs failed", e),
        },
        Command::Inventory => match inventory(rt).await {
            Ok(report) => ok(
                cmd_id,
                serde_json::to_value(report).unwrap_or_default(),
                "inventory ok",
            ),
            Err(e) => err(cmd_id, "inventory failed", e),
        },
    }
}

/// What is on this host: unit states of the known sites, site trees,
/// Caddy route ids and `site_*` databases.
pub async fn inventory<H, P, C>(rt: &NativeRuntime<H, P, C>) -> anyhow::Result<InventoryReport>
where
    H: Helper,
    P: PostgresAdmin + Send + Sync,
    C: CaddyAdmin,
{
    let state = crate::state::StateFile::load(&rt.config().state_path)?;
    let mut units = Vec::new();
    for short in state.sites.keys() {
        units.push(unit_state(rt, short).await);
    }
    Ok(InventoryReport {
        units,
        sites_on_disk: rt.sites_on_disk(),
        routes: rt.caddy().list_route_ids().await?,
        databases: rt.postgres().list_databases().await?,
    })
}

/// One site's unit state (`unknown` when the helper cannot say).
pub async fn unit_state<H, P, C>(rt: &NativeRuntime<H, P, C>, short: &str) -> UnitState
where
    H: Helper,
{
    let status = rt
        .status_inner(short)
        .await
        .unwrap_or_else(|_| RuntimeStatus {
            sub: "unknown".to_string(),
            ..Default::default()
        });
    UnitState {
        short_id: short.to_string(),
        active: status.active,
        sub: status.sub,
        pid: status.pid,
        restarts: status.n_restarts,
    }
}

/// Snake-case kind tag (proto's [`Command::kind`], for transcripts).
pub fn command_kind(cmd: &Command) -> &'static str {
    cmd.kind()
}
