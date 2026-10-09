//! Provision state machine (spec §5).
//!
//! Named steps: `validate`, `create_database`, `install`, `write_unit`,
//! `write_env`, `start`, `health`, `set_route`, `verify_status`,
//! `finish`. Any failure marks the site `failed` with the step name.
//! Re-running from `failed` restarts from scratch: every agent command
//! is idempotent, so this is safe.
//!
//! Step notes:
//! - `validate` does all pre-agent work (manifest/plan/capacity checks,
//!   env resolution, sealed env store). Image manifests are rejected
//!   here, before any command is issued.
//! - `health` calls `Health`; `verify_status` calls `Status` once more
//!   after `SetRoute` (Review-Focus #4: a crash between health and
//!   routing must end `failed`, with a `RemoveRoute` issued).
//! - `set_route` also issues the second `WriteEnv` without `once` vars,
//!   so one-shot secrets leave the agent's env file after first boot
//!   (the control plane keeps them sealed).

use setu_common::AppError;
use setu_proto::Command;

use crate::{agent_step, call_agent, fail, log_step, prepare_site, JobCtx};

/// Provision the site: full step sequence, ending `running`.
pub async fn run(ctx: &JobCtx, site_id: &str) -> Result<(), AppError> {
    let site = ctx.db.sites().get(site_id).await?;
    if site.status != "provisioning" && site.status != "failed" {
        return Err(AppError::validation(format!(
            "site {site_id} is not provisioning (status: {})",
            site.status
        )));
    }
    if site.status == "failed" {
        ctx.db
            .sites()
            .set_status(site_id, "provisioning", None)
            .await?;
    }
    // Re-read after the retry reset above.
    let site = ctx.db.sites().get(site_id).await?;
    let server_id = site.server_id.clone();
    let domain = site.domain.clone();
    // Route id `setu-site-<short>` on the agent (ledger R71).
    let route_short = Some(site.short_id.clone());
    let port = site.port;

    let prep = match prepare_site(ctx, &site).await {
        Ok(prep) => {
            log_step(ctx, "validate", true, None).await;
            prep
        }
        Err(e) => return Err(fail(ctx, site_id, "validate", e).await),
    };

    agent_step(
        ctx,
        site_id,
        "create_database",
        &server_id,
        Command::CreateDatabase {
            db: site.db_name.clone(),
            role: site.db_role.clone(),
            password: prep.db_password.clone(),
        },
    )
    .await?;
    agent_step(
        ctx,
        site_id,
        "install",
        &server_id,
        Command::Install {
            site: prep.spec.clone(),
        },
    )
    .await?;
    agent_step(
        ctx,
        site_id,
        "write_unit",
        &server_id,
        Command::WriteUnit {
            site: prep.spec.clone(),
        },
    )
    .await?;
    agent_step(
        ctx,
        site_id,
        "write_env",
        &server_id,
        Command::WriteEnv {
            site: prep.spec.clone(),
            env: prep.env_all.clone(),
        },
    )
    .await?;
    agent_step(
        ctx,
        site_id,
        "start",
        &server_id,
        Command::Start {
            site_id: site.id.clone(),
        },
    )
    .await?;
    agent_step(
        ctx,
        site_id,
        "health",
        &server_id,
        Command::Health {
            site_id: site.id.clone(),
            port: port as u16,
            path: prep.manifest.app.health.clone(),
            timeout_s: u64::from(prep.manifest.app.startup_grace_s),
        },
    )
    .await?;

    // `set_route`, then drop `once` secrets from the agent env file with
    // a second `WriteEnv` (folded into this step to keep the named steps
    // exact; see module docs).
    match call_agent(
        ctx,
        &server_id,
        Command::SetRoute {
            domain: domain.clone(),
            port: port as u16,
            site_short_id: route_short.clone(),
        },
    )
    .await
    {
        Ok(result) if result.ok => {
            if prep.has_once {
                match call_agent(
                    ctx,
                    &server_id,
                    Command::WriteEnv {
                        site: prep.spec.clone(),
                        env: prep.env_persistent.clone(),
                    },
                )
                .await
                {
                    Ok(r) if r.ok => {}
                    Ok(r) => {
                        let msg = r
                            .error
                            .unwrap_or_else(|| "agent command failed".to_string());
                        let _ = call_agent(
                            ctx,
                            &server_id,
                            Command::RemoveRoute {
                                domain: domain.clone(),
                                site_short_id: route_short.clone(),
                            },
                        )
                        .await;
                        return Err(fail(
                            ctx,
                            site_id,
                            "set_route",
                            AppError::new(
                                setu_common::ErrorCode::Internal,
                                format!("clearing once-secrets failed: {msg}"),
                            ),
                        )
                        .await);
                    }
                    Err(e) => {
                        let _ = call_agent(
                            ctx,
                            &server_id,
                            Command::RemoveRoute {
                                domain: domain.clone(),
                                site_short_id: route_short.clone(),
                            },
                        )
                        .await;
                        return Err(fail(ctx, site_id, "set_route", e.into()).await);
                    }
                }
            }
            log_step(ctx, "set_route", true, None).await;
        }
        Ok(result) => {
            let msg = result
                .error
                .unwrap_or_else(|| "agent command failed".to_string());
            return Err(fail(
                ctx,
                site_id,
                "set_route",
                AppError::new(setu_common::ErrorCode::Internal, msg),
            )
            .await);
        }
        Err(e) => return Err(fail(ctx, site_id, "set_route", e.into()).await),
    }

    // The app may have crashed after health passed: verify it is still
    // up now that the route exists. On inactive, remove the route again
    // so no traffic reaches a dead site.
    match call_agent(
        ctx,
        &server_id,
        Command::Status {
            site_id: site.id.clone(),
        },
    )
    .await
    {
        Ok(result)
            if result.ok
                && result
                    .output
                    .get("active")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false) =>
        {
            log_step(ctx, "verify_status", true, None).await;
        }
        Ok(result) => {
            let _ = call_agent(
                ctx,
                &server_id,
                Command::RemoveRoute {
                    domain: domain.clone(),
                    site_short_id: route_short.clone(),
                },
            )
            .await;
            let msg = if result.ok {
                "site is not active after routing".to_string()
            } else {
                result
                    .error
                    .unwrap_or_else(|| "agent command failed".to_string())
            };
            return Err(fail(
                ctx,
                site_id,
                "verify_status",
                AppError::new(setu_common::ErrorCode::Internal, msg),
            )
            .await);
        }
        Err(e) => {
            let _ = call_agent(
                ctx,
                &server_id,
                Command::RemoveRoute {
                    domain: domain.clone(),
                    site_short_id: route_short.clone(),
                },
            )
            .await;
            return Err(fail(ctx, site_id, "verify_status", e.into()).await);
        }
    }

    match ctx.db.sites().set_status(site_id, "running", None).await {
        Ok(_) => {
            log_step(ctx, "finish", true, None).await;
            Ok(())
        }
        Err(e) => Err(fail(ctx, site_id, "finish", e).await),
    }
}
