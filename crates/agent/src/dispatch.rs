//! The per-variant executor: every `Command` variant run against a
//! `NativeRuntime`. Handlers are idempotent (check state, then act) so the
//! control plane may redeliver freely after timeouts. Expiry, dedupe,
//! per-site ordering, redaction and persistence live in
//! [`crate::commands::handle`], which calls [`execute`].
//!
//! Result logs never carry env values or passwords.

use setu_proto::{Command, CommandResult, InventoryReport, LogsOutput, UnitState};

use crate::caddy::CaddyAdmin;
use crate::helper_client::Helper;
use crate::postgres::PostgresAdmin;
use crate::runtime::{NativeRuntime, RuntimeStatus};

/// Make `short`'s `www.` redirect route match the state: present when
/// [`crate::state::State::www_host`] names one, removed otherwise. Returns
/// the www name served, if any. Callers hold the route lock.
async fn apply_www<H, P, C>(
    rt: &NativeRuntime<H, P, C>,
    short: &str,
) -> anyhow::Result<Option<String>>
where
    H: Helper,
    C: CaddyAdmin,
{
    let www_id = crate::caddy::site_www_route_id(short)?;
    let (www, domain) = rt.state().read(|st| {
        (
            st.www_host(short),
            st.sites.get(short).and_then(|r| r.domain.clone()),
        )
    });
    match (&www, &domain) {
        (Some(_), Some(domain)) => rt.caddy().upsert_www_route(&www_id, domain).await?,
        _ => rt.caddy().remove_route(&www_id).await?,
    }
    Ok(www)
}

/// Make the alias routes of `short` match the state: one redirect route
/// per live `redirect` alias, none left from removed ones. Returns the live
/// aliases. Callers hold the route lock; the `serve` hosts ride in the site
/// route itself (see [`serve_hosts`]).
async fn apply_aliases<H, P, C>(
    rt: &NativeRuntime<H, P, C>,
    short: &str,
) -> anyhow::Result<Vec<setu_proto::Alias>>
where
    H: Helper,
    C: CaddyAdmin,
{
    let (live, domain) = rt.state().read(|st| {
        (
            st.live_aliases(short),
            st.sites.get(short).and_then(|r| r.domain.clone()),
        )
    });
    let prefix = format!("{}-alias-", crate::caddy::site_route_id(short)?);
    let mut wanted = std::collections::BTreeSet::new();
    if let Some(domain) = &domain {
        for (n, a) in live
            .iter()
            .filter(|a| a.mode == setu_proto::AliasMode::Redirect)
            .enumerate()
        {
            let id = crate::caddy::site_alias_route_id(short, n)?;
            rt.caddy().upsert_alias_route(&id, domain, &a.host).await?;
            wanted.insert(id);
        }
    }
    for id in rt.caddy().list_route_ids().await? {
        if id.starts_with(&prefix) && !wanted.contains(&id) {
            rt.caddy().remove_route(&id).await?;
        }
    }
    Ok(live)
}

/// Re-apply everything of `short` that depends on who holds a hostname:
/// its site route (the `serve` hosts), `www.` redirect and alias routes.
/// Used for the neighbours of a `SetRoute`/`RemoveRoute`, so a host that
/// changes hands (an alias moved between sites, a name freed) is served by
/// its new holder without waiting for a reconcile. Callers hold the route
/// lock.
async fn reapply_site<H, P, C>(rt: &NativeRuntime<H, P, C>, short: &str) -> anyhow::Result<()>
where
    H: Helper,
    C: CaddyAdmin,
{
    let Some((domain, port, basic_auth, noindex, serve)) = rt.state().read(|st| {
        let r = st.sites.get(short)?;
        Some((
            r.domain.clone()?,
            r.port,
            r.basic_auth.clone(),
            r.noindex,
            crate::state::serve_hosts(st, short),
        ))
    }) else {
        return Ok(());
    };
    if port != 0 {
        let id = crate::caddy::site_route_id(short)?;
        rt.caddy()
            .upsert_route_hosts(
                &id,
                &domain,
                &serve,
                &crate::caddy::site_upstream(port),
                basic_auth.as_ref().map(|(u, h)| (u.as_str(), h.as_str())),
                noindex,
            )
            .await?;
    }
    apply_www(rt, short).await?;
    apply_aliases(rt, short).await?;
    Ok(())
}

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

/// Execute one command to a result. Covers every `Command` variant; there
/// is no fallthrough arm, so adding a variant breaks compilation here.
#[allow(clippy::too_many_lines)] // reviewed: kept as-is (R150)
pub async fn execute<H, P, C>(
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
                Ok(()) => {
                    record_db(rt, db, Some(db.clone()));
                    ok(cmd_id, serde_json::json!({"db": db}), "create_database ok")
                }
                Err(e) => err(cmd_id, "create_database failed", e),
            }
        }
        Command::DropDatabase { db, role } => match rt.postgres().drop(db, role).await {
            Ok(()) => {
                record_db(rt, db, None);
                ok(cmd_id, serde_json::json!({"db": db}), "drop_database ok")
            }
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
            basic_auth,
            www_redirect,
            force_reload,
            aliases,
            noindex,
        } => {
            let id = match crate::caddy::site_route_id(site_short_id) {
                Ok(id) => id,
                Err(e) => return err(cmd_id, "set_route failed", e),
            };
            let upstream = crate::caddy::site_upstream(*port);
            let _routes = rt.route_lock().lock().await;
            // Hosts this site held or now claims: other sites' routes that
            // depend on them are re-applied below.
            let mut names = vec![domain.clone()];
            names.extend(aliases.iter().map(|a| a.host.clone()));
            rt.state().read(|st| {
                if let Some(r) = st.sites.get(site_short_id) {
                    names.extend(r.aliases.iter().map(|a| a.host.clone()));
                }
            });
            // Recorded first: reconcile rebuilds the route from state.
            if let Err(e) = rt.update_state(site_short_id, |r| {
                r.domain = Some(domain.clone());
                r.port = *port;
                r.basic_auth = basic_auth
                    .as_ref()
                    .map(|a| (a.username.clone(), a.password_hash.clone()));
                r.www_redirect = *www_redirect;
                r.aliases.clone_from(aliases);
                r.noindex = *noindex;
            }) {
                return err(cmd_id, "set_route failed", e);
            }
            let serve = rt
                .state()
                .read(|st| crate::state::serve_hosts(st, site_short_id));
            if let Err(e) = rt
                .caddy()
                .upsert_route_hosts(
                    &id,
                    domain,
                    &serve,
                    &upstream,
                    basic_auth
                        .as_ref()
                        .map(|a| (a.username.as_str(), a.password_hash.as_str())),
                    *noindex,
                )
                .await
            {
                return err(cmd_id, "set_route failed", e);
            }
            if *force_reload {
                if let Err(e) = rt.caddy().reload_route(&id).await {
                    return err(cmd_id, "set_route failed", e);
                }
            }
            // The `www.` name of an apex domain redirects to it; opting out,
            // a domain that has none, or another site serving that name
            // removes a redirect left from before.
            let www = match apply_www(rt, site_short_id).await {
                Ok(w) => w,
                Err(e) => return err(cmd_id, "set_route failed", e),
            };
            let live = match apply_aliases(rt, site_short_id).await {
                Ok(l) => l,
                Err(e) => return err(cmd_id, "set_route failed", e),
            };
            let mut neighbours = rt.state().read(|st| {
                let mut n = st.www_neighbours(site_short_id, domain);
                n.extend(st.host_neighbours(site_short_id, &names));
                n
            });
            neighbours.sort();
            neighbours.dedup();
            for short in &neighbours {
                if let Err(e) = reapply_site(rt, short).await {
                    return err(cmd_id, "set_route failed", e);
                }
            }
            ok(
                cmd_id,
                serde_json::json!({"domain": domain, "route_id": id, "www": www, "aliases": live.len(), "noindex": noindex}),
                "set_route ok",
            )
        }
        Command::RemoveRoute {
            site_short_id,
            domain,
        } => {
            let id = match crate::caddy::site_route_id(site_short_id) {
                Ok(id) => id,
                Err(e) => return err(cmd_id, "remove_route failed", e),
            };
            let _routes = rt.route_lock().lock().await;
            let known = rt.state().read(|st| st.sites.contains_key(site_short_id));
            // The names this site held (domain and aliases) are free again.
            let mut names = vec![domain.clone()];
            rt.state().read(|st| {
                if let Some(r) = st.sites.get(site_short_id) {
                    names.extend(r.aliases.iter().map(|a| a.host.clone()));
                }
            });
            if known {
                if let Err(e) = rt.update_state(site_short_id, |r| {
                    r.domain = None;
                    r.aliases.clear();
                }) {
                    return err(cmd_id, "remove_route failed", e);
                }
            }
            // The name this site served is free again: a neighbour's
            // `www.` redirect may take it.
            let mut neighbours = rt.state().read(|st| {
                let mut n = st.www_neighbours(site_short_id, domain);
                n.extend(st.host_neighbours(site_short_id, &names));
                n
            });
            neighbours.sort();
            neighbours.dedup();
            let removed = match crate::caddy::site_www_route_id(site_short_id) {
                Ok(www_id) => rt.caddy().remove_route(&www_id).await,
                Err(e) => Err(e),
            };
            let removed = match removed {
                Ok(()) => {
                    let prefix = format!("{id}-alias-");
                    match rt.caddy().list_route_ids().await {
                        Ok(ids) => {
                            let mut res = Ok(());
                            for alias_id in ids.iter().filter(|i| i.starts_with(&prefix)) {
                                if let Err(e) = rt.caddy().remove_route(alias_id).await {
                                    res = Err(e);
                                    break;
                                }
                            }
                            res
                        }
                        Err(e) => Err(e),
                    }
                }
                Err(e) => Err(e),
            };
            let removed = match removed {
                Ok(()) => rt.caddy().remove_route(&id).await,
                Err(e) => Err(e),
            };
            let removed = match removed {
                Ok(()) => {
                    let mut res = Ok(());
                    for short in &neighbours {
                        if let Err(e) = reapply_site(rt, short).await {
                            res = Err(e);
                            break;
                        }
                    }
                    res
                }
                Err(e) => Err(e),
            };
            match removed {
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
        Command::FetchSystemLogs {
            unit,
            cursor,
            lines,
        } => match rt
            .system_logs_inner(*unit, cursor.as_deref(), (*lines).clamp(1, 5000))
            .await
        {
            Ok(stream) => ok(
                cmd_id,
                serde_json::to_value(LogsOutput {
                    lines: stream.lines,
                    cursor: stream.next,
                })
                .unwrap_or_default(),
                "fetch_system_logs ok",
            ),
            Err(e) => err(cmd_id, "fetch_system_logs failed", e),
        },
        Command::CertStatus { site, domain } => {
            if crate::caddy::site_route_id(site).is_err() {
                return err(
                    cmd_id,
                    "cert_status failed",
                    anyhow::anyhow!("invalid site {site}"),
                );
            }
            if !crate::caddy::valid_domain(domain) {
                return err(
                    cmd_id,
                    "cert_status failed",
                    anyhow::anyhow!("invalid domain {domain}"),
                );
            }
            let out = crate::cert::probe(crate::cert::LOCAL_HTTPS, domain).await;
            ok(
                cmd_id,
                serde_json::to_value(out).unwrap_or_default(),
                "cert_status ok",
            )
        }
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
/// Caddy route ids, `site_*` databases and listening TCP ports.
pub async fn inventory<H, P, C>(rt: &NativeRuntime<H, P, C>) -> anyhow::Result<InventoryReport>
where
    H: Helper,
    P: PostgresAdmin + Send + Sync,
    C: CaddyAdmin,
{
    let state = rt.state().snapshot();
    let mut units = Vec::new();
    for short in state.sites.keys() {
        units.push(unit_state(rt, short).await);
    }
    Ok(InventoryReport {
        units,
        sites_on_disk: rt.sites_on_disk(),
        routes: rt.caddy().list_route_ids().await?,
        databases: rt.postgres().list_databases().await?,
        listening_ports: crate::netstat::listening_ports(),
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

/// `site_<short>` databases are recorded on their site (state `db`).
fn record_db<H, P, C>(rt: &NativeRuntime<H, P, C>, db: &str, value: Option<String>) {
    let Some(short) = db
        .strip_prefix("site_")
        .filter(|s| setu_common::is_short_id(s))
    else {
        return;
    };
    let known = rt.state().read(|st| st.sites.contains_key(short));
    if known {
        if let Err(e) = rt.state().update_site(short, |r| r.db = value) {
            tracing::warn!(error = %e, "cannot record database in state");
        }
    }
}

/// Snake-case kind tag (proto's [`Command::kind`], for transcripts).
pub fn command_kind(cmd: &Command) -> &'static str {
    cmd.kind()
}
