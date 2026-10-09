//! Command dispatch: every `Command` variant handled against a
//! `NativeRuntime`. Handlers are idempotent (check state, then act) so the
//! control plane may redeliver freely after timeouts.
//!
//! Result logs never carry env values or passwords.

use setu_proto::{Command, CommandResult};

use crate::caddy::CaddyAdmin;
use crate::helper::unit_name;
use crate::helper_client::Helper;
use crate::postgres::PostgresAdmin;
use crate::runtime::NativeRuntime;

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
        Command::CreateDatabase { db, role, password } => {
            match rt.postgres().create_db_and_role(db, role, password).await {
                Ok(()) => ok(cmd_id, serde_json::json!({"db": db}), "create_database ok"),
                Err(e) => err(cmd_id, "create_database failed", e),
            }
        }
        Command::DropDatabase { db, role } => {
            match rt.postgres().drop_db_and_role(db, role).await {
                Ok(()) => ok(cmd_id, serde_json::json!({"db": db}), "drop_database ok"),
                Err(e) => err(cmd_id, "drop_database failed", e),
            }
        }
        Command::Install { site } => match rt.install_version(site).await {
            Ok(version) => ok(
                cmd_id,
                serde_json::json!({"version": version}),
                "install ok",
            ),
            Err(e) => err(cmd_id, "install failed", e),
        },
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
        Command::Start { site_id } => match rt.start_inner(site_id).await {
            Ok(()) => ok(cmd_id, serde_json::json!({}), "start ok"),
            Err(e) => err(cmd_id, "start failed", e),
        },
        Command::Stop { site_id } => match rt.stop_inner(site_id).await {
            Ok(()) => ok(cmd_id, serde_json::json!({}), "stop ok"),
            Err(e) => err(cmd_id, "stop failed", e),
        },
        Command::Restart { site_id } => match rt.restart_inner(site_id).await {
            Ok(()) => ok(cmd_id, serde_json::json!({}), "restart ok"),
            Err(e) => err(cmd_id, "restart failed", e),
        },
        Command::Status { site_id } => match rt.status_inner(site_id).await {
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
        Command::Health {
            site_id,
            port,
            path,
            timeout_s,
        } => {
            let timeout = std::time::Duration::from_secs((*timeout_s).clamp(1, 300));
            match rt.health_inner(site_id, *port, path, timeout).await {
                Ok(status) => ok(cmd_id, serde_json::json!({"http": status}), "health ok"),
                Err(e) => err(cmd_id, "health failed", e),
            }
        }
        Command::SetRoute {
            domain,
            port,
            site_short_id,
        } => {
            let id = match route_id_for(site_short_id.as_deref()) {
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
            domain,
            site_short_id,
        } => {
            let id = match route_id_for(site_short_id.as_deref()) {
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
            dest,
        } => match crate::backup::backup_site(rt, site, db, dirs, dest).await {
            Ok(dir) => ok(cmd_id, serde_json::json!({"dest": dir}), "backup ok"),
            Err(e) => err(cmd_id, "backup failed", e),
        },
        Command::Restore { site, db, src } => {
            match crate::backup::restore_site(rt, site, db, src).await {
                Ok(()) => ok(cmd_id, serde_json::json!({"src": src}), "restore ok"),
                Err(e) => err(cmd_id, "restore failed", e),
            }
        }
        Command::Remove { site } => match rt.remove_inner(&site.site_id).await {
            Ok(()) => ok(cmd_id, serde_json::json!({}), "remove ok"),
            Err(e) => err(cmd_id, "remove failed", e),
        },
        Command::SwitchVersion { site_id, version } => {
            match rt.switch_version_inner(site_id, version).await {
                Ok(changed) => ok(
                    cmd_id,
                    serde_json::json!({"version": version, "changed": changed}),
                    "switch_version ok",
                ),
                Err(e) => err(cmd_id, "switch_version failed", e),
            }
        }
        Command::FetchLogs { site_id, lines } => {
            match rt.logs_inner(site_id, None, (*lines).clamp(1, 5000)).await {
                Ok(stream) => ok(
                    cmd_id,
                    serde_json::json!({"lines": stream.lines, "cursor": stream.next}),
                    "fetch_logs ok",
                ),
                Err(e) => err(cmd_id, "fetch_logs failed", e),
            }
        }
        Command::Inventory => {
            let state = crate::state::StateFile::load(&rt.config().state_path);
            match state {
                Ok(s) => {
                    let sites: Vec<serde_json::Value> = s
                        .sites
                        .iter()
                        .map(|(id, e)| {
                            let unit = unit_name(&setu_common::short_id(id).to_lowercase());
                            serde_json::json!({
                                "site_id": id,
                                "unit": unit,
                                "port": e.port,
                                "db": e.db,
                                "version": e.version,
                            })
                        })
                        .collect();
                    ok(cmd_id, serde_json::json!({"sites": sites}), "inventory ok")
                }
                Err(e) => err(cmd_id, "inventory failed", e),
            }
        }
    }
}

/// Route id `setu-site-<short>`; a route command without the site's
/// short id is refused rather than keyed off the domain (ledger R71).
fn route_id_for(short: Option<&str>) -> anyhow::Result<String> {
    let short = short.ok_or_else(|| anyhow::anyhow!("route command without site_short_id"))?;
    crate::caddy::site_route_id(short)
}

/// Snake-case kind tag, mirroring the proto `kind` tag (for transcripts).
pub fn command_kind(cmd: &Command) -> &'static str {
    match cmd {
        Command::CreateDatabase { .. } => "create_database",
        Command::DropDatabase { .. } => "drop_database",
        Command::Install { .. } => "install",
        Command::WriteUnit { .. } => "write_unit",
        Command::WriteEnv { .. } => "write_env",
        Command::Start { .. } => "start",
        Command::Stop { .. } => "stop",
        Command::Restart { .. } => "restart",
        Command::Status { .. } => "status",
        Command::Health { .. } => "health",
        Command::SetRoute { .. } => "set_route",
        Command::RemoveRoute { .. } => "remove_route",
        Command::Backup { .. } => "backup",
        Command::Restore { .. } => "restore",
        Command::Remove { .. } => "remove",
        Command::SwitchVersion { .. } => "switch_version",
        Command::FetchLogs { .. } => "fetch_logs",
        Command::Inventory => "inventory",
    }
}
