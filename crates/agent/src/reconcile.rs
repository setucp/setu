//! Reconcile Caddy against `state.json` (spec §4): on start and every
//! `reconcile_secs` (default 300, `SETU_AGENT_RECONCILE_SECS`).
//!
//! - every site with a recorded domain gets `setu-site-<short>` →
//!   `127.0.0.1:<port>` (upsert: PATCH when present, POST when not), and
//!   the panel route from `agent.toml` gets `setu-panel`, upserted first
//!   and moved to the front of the routes array when it is not there
//!   (Caddy routes are first-match);
//! - `setu-site-*` routes whose site is not in state (or has no domain)
//!   are removed; routes Setu did not write are never touched;
//! - unknown site trees and `site_*` databases are **reported** (logged
//!   and returned in the inventory), never deleted.
//!
//! This repairs a Caddy whose autosave was lost.

use std::collections::BTreeSet;

use setu_proto::{InventoryReport, UnitState};

use crate::caddy::{site_route_id, site_upstream, CaddyAdmin, PANEL_ROUTE_ID};
use crate::config::AgentConfig;
use crate::helper::Verb;
use crate::helper_client::Helper;
use crate::postgres::PostgresAdmin;
use crate::runtime::{NativeRuntime, RuntimeStatus};
use crate::state::State;

/// One pass over `state`. Route failures are logged per route and do
/// not stop the pass; listing routes failing is an error (nothing can be
/// judged stale without the list).
pub async fn run<H, C, P>(
    state: &State,
    config: &AgentConfig,
    helper: &H,
    caddy: &C,
    postgres: &P,
) -> anyhow::Result<InventoryReport>
where
    H: Helper + ?Sized,
    C: CaddyAdmin + ?Sized,
    P: PostgresAdmin + ?Sized,
{
    // The panel first, and kept first: Caddy routes are first-match, so
    // no site route may precede it (R130).
    if let Some(panel) = &config.panel {
        if let Err(e) = caddy
            .upsert_route(PANEL_ROUTE_ID, &panel.host, &panel.upstream)
            .await
        {
            tracing::warn!(error = %e, "reconcile: cannot upsert the panel route");
        }
    }
    let mut wanted = BTreeSet::new();
    for (short, site) in &state.sites {
        let (Some(domain), Ok(id)) = (&site.domain, site_route_id(short)) else {
            continue;
        };
        if site.port == 0 {
            continue;
        }
        wanted.insert(id.clone());
        if let Err(e) = caddy
            .upsert_route(&id, domain, &site_upstream(site.port))
            .await
        {
            tracing::warn!(route = %id, error = %e, "reconcile: cannot upsert route");
        }
    }
    if let Err(e) = caddy.move_route_first(PANEL_ROUTE_ID).await {
        tracing::warn!(error = %e, "reconcile: cannot move the panel route first");
    }
    for id in caddy.list_route_ids().await? {
        if id.starts_with("setu-site-") && !wanted.contains(&id) {
            match caddy.remove_route(&id).await {
                Ok(()) => tracing::info!(route = %id, "reconcile: removed stale route"),
                Err(e) => tracing::warn!(route = %id, error = %e, "reconcile: cannot remove"),
            }
        }
    }

    let mut units = Vec::new();
    for short in state.sites.keys() {
        let status = helper
            .call(
                Verb::Unit,
                short,
                serde_json::json!({ "op": "show" }),
                crate::helper_client::HelperIo::none(),
            )
            .await
            .map_or_else(
                |_| RuntimeStatus {
                    sub: "unknown".to_string(),
                    ..Default::default()
                },
                |o| RuntimeStatus::from_show(&o.json),
            );
        units.push(UnitState {
            short_id: short.clone(),
            active: status.active,
            sub: status.sub,
            pid: status.pid,
            restarts: status.n_restarts,
        });
    }
    let sites_on_disk = sites_on_disk(&config.sites_root);
    let databases = postgres.list_databases().await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "reconcile: cannot list databases");
        Vec::new()
    });
    for short in sites_on_disk
        .iter()
        .filter(|s| !state.sites.contains_key(*s))
    {
        tracing::warn!(site = %short, "reconcile: unknown site tree (left in place)");
    }
    let known_dbs: BTreeSet<String> = state
        .sites
        .iter()
        .flat_map(|(short, s)| [s.db.clone(), Some(format!("site_{short}"))])
        .flatten()
        .collect();
    for db in databases.iter().filter(|d| !known_dbs.contains(*d)) {
        tracing::warn!(db = %db, "reconcile: unknown database (left in place)");
    }
    Ok(InventoryReport {
        units,
        sites_on_disk,
        routes: caddy.list_route_ids().await?,
        databases,
        listening_ports: crate::netstat::listening_ports(),
    })
}

/// [`run`] over a runtime's own parts, holding the route lock.
pub async fn run_runtime<H, P, C>(rt: &NativeRuntime<H, P, C>) -> anyhow::Result<InventoryReport>
where
    H: Helper,
    P: PostgresAdmin + Send + Sync,
    C: CaddyAdmin,
{
    let _routes = rt.route_lock().lock().await;
    let state = rt.state().snapshot();
    run(&state, rt.config(), rt.helper(), rt.caddy(), rt.postgres()).await
}

/// Short ids with a tree under `sites_root`.
pub fn sites_on_disk(sites_root: &std::path::Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(sites_root)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| setu_common::is_short_id(n))
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}
