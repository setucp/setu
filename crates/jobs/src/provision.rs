//! Provision state machine (spec §6), twelve named steps resumed from
//! the job's cursor:
//!
//! `validate`, `site_create`, `create_database`, `install` (holding one
//! of the server's two install slots), `switch_version`, `write_unit`,
//! `write_env` (with `once` values: first boot only), `start`, `health`,
//! `set_route`, `verify` (`status` active [`JobCtx::verify_delay`] after
//! the route, else `remove_route` and fail), `finish` (`write_env`
//! again WITHOUT `once` values, then `running`).
//!
//! The env, port, short id and database names were allocated at `POST
//! /sites` in one transaction; provision only READS the stored env and
//! never regenerates it, so a retry (a new job, from step 1) sends the
//! same values and the same database password. Any failure marks the
//! site `failed` with the step name; artefacts are kept.

use setu_common::{AppError, ErrorCode};
use setu_db::{Plan, Site};
use setu_manifest::Manifest;
use setu_proto::{Command, EnvVar, InventoryReport, SiteSpec};

use crate::{
    binary_ref, build_spec, load_manifest, mark_failed, site_env, without_once, JobCtx, Steps,
};

/// The steps, in order (spec §6).
pub const STEPS: &[&str] = &[
    "validate",
    "site_create",
    "create_database",
    "install",
    "switch_version",
    "write_unit",
    "write_env",
    "start",
    "health",
    "set_route",
    "verify",
    "finish",
];

/// Everything the agent steps send, read from storage (no side effects).
struct Prep {
    spec: SiteSpec,
    binary: setu_proto::BinaryRef,
    env_first_boot: Vec<EnvVar>,
    env_steady: Vec<EnvVar>,
    db_password: String,
}

/// Provision the site, ending `running`.
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
    let steps = Steps::run_from(ctx.cursor().await.as_deref(), STEPS);
    match machine(ctx, &site, &steps).await {
        Ok(()) => Ok(()),
        Err((step, e)) => Err(mark_failed(ctx, site_id, step, e).await),
    }
}

#[allow(clippy::too_many_lines)] // reviewed: kept as-is (R150)
async fn machine(ctx: &JobCtx, site: &Site, steps: &Steps) -> Result<(), (&'static str, AppError)> {
    let server = site.server_id.as_str();
    let short = site.short_id.clone();

    let prep = match steps
        .run(ctx, "validate", validate(ctx, site))
        .await
        .map_err(|e| ("validate", e))?
    {
        Some(prep) => prep,
        // Resumed past `validate`: rebuild the (pure) inputs.
        None => prepare(ctx, site).await.map_err(|e| ("validate", e))?.0,
    };

    macro_rules! agent_step {
        ($name:literal, $cmd:expr) => {
            steps
                .run(ctx, $name, async { ctx.agent_ok(server, $cmd).await })
                .await
                .map_err(|e| ($name, e))?
        };
    }

    agent_step!(
        "site_create",
        Command::SiteCreate {
            site: prep.spec.clone()
        }
    );
    agent_step!(
        "create_database",
        Command::CreateDatabase {
            db: site.db_name.clone(),
            role: site.db_role.clone(),
            password: prep.db_password.clone(),
        }
    );
    agent_step!(
        "install",
        Command::Install {
            site: short.clone(),
            binary: prep.binary.clone(),
        }
    );
    agent_step!(
        "switch_version",
        Command::SwitchVersion {
            site: short.clone(),
            version: prep.spec.version.clone(),
        }
    );
    agent_step!(
        "write_unit",
        Command::WriteUnit {
            site: prep.spec.clone()
        }
    );
    agent_step!(
        "write_env",
        Command::WriteEnv {
            site: short.clone(),
            env: prep.env_first_boot.clone(),
        }
    );
    agent_step!(
        "start",
        Command::Start {
            site: short.clone()
        }
    );
    agent_step!(
        "health",
        Command::Health {
            site: prep.spec.clone()
        }
    );
    agent_step!(
        "set_route",
        Command::SetRoute {
            site_short_id: short.clone(),
            domain: site.domain.clone(),
            port: prep.spec.port,
        }
    );
    steps
        .run(ctx, "verify", verify(ctx, site))
        .await
        .map_err(|e| ("verify", e))?;
    steps
        .run(ctx, "finish", async {
            // Health passed, so the app consumed its `once` values: they
            // leave the env file now (spec §2; the control plane keeps
            // them sealed for reveal).
            ctx.agent_ok(
                server,
                Command::WriteEnv {
                    site: short.clone(),
                    env: prep.env_steady.clone(),
                },
            )
            .await?;
            ctx.db.sites().set_status(&site.id, "running", None).await?;
            Ok(())
        })
        .await
        .map_err(|e| ("finish", e))?;
    Ok(())
}

/// Step 1: domain unique (excluding this site), plan allows the app,
/// manifest is a binary, server online and not cordoned, port free on
/// the host. Capacity was admitted at creation, under the server lock.
async fn validate(ctx: &JobCtx, site: &Site) -> Result<Prep, AppError> {
    let (prep, plan, _manifest) = prepare(ctx, site).await?;
    if ctx
        .db
        .sites()
        .by_domain_excluding(&site.domain, &site.id)
        .await?
        .is_some()
    {
        return Err(AppError::conflict(format!(
            "domain {} is used by another site",
            site.domain
        )));
    }
    if !plan.allows(&site.app_id) {
        return Err(AppError::validation(format!(
            "plan does not allow app {}",
            site.app_id
        )));
    }
    let server = ctx.db.servers().get(&site.server_id).await?;
    if server.status(ctx.clock.now()) != setu_db::ServerStatus::Online {
        return Err(AppError::validation(format!(
            "server {} is offline",
            server.name
        )));
    }
    if server.cordoned {
        return Err(AppError::validation(format!(
            "server {} is cordoned",
            server.name
        )));
    }
    let inventory: InventoryReport =
        serde_json::from_value(ctx.agent_ok(&site.server_id, Command::Inventory).await?)
            .map_err(|_| AppError::new(ErrorCode::Internal, "agent sent a bad inventory"))?;
    // The site's own unit may hold the port on a retry after `start`.
    let ours_active = inventory
        .units
        .iter()
        .any(|u| u.short_id == site.short_id && u.active);
    if inventory.listening_ports.contains(&prep.spec.port) && !ours_active {
        return Err(AppError::conflict(format!(
            "port {} is already in use on server {}",
            prep.spec.port, server.name
        )));
    }
    Ok(prep)
}

/// Load the manifest and plan and read the stored env (never
/// regenerated); image manifests and an env missing a rule are refused.
async fn prepare(ctx: &JobCtx, site: &Site) -> Result<(Prep, Plan, Manifest), AppError> {
    let manifest = load_manifest(&ctx.db, &site.app_id, &site.app_version).await?;
    let plan = ctx.db.plans().get(&site.plan_id).await?;
    let spec = build_spec(&plan, site, &manifest)?;
    let binary = binary_ref(&manifest)?;
    let vars = site_env(ctx, site)?;
    if let Some(missing) = manifest
        .env
        .keys()
        .find(|name| !vars.iter().any(|v| &v.name == *name))
    {
        return Err(AppError::validation(format!(
            "env {missing} was not generated at creation"
        )));
    }
    let db_password = crate::db_password(&manifest, &vars);
    ctx.add_secret(&db_password);
    let prep = Prep {
        spec,
        binary,
        env_first_boot: crate::to_proto_envs(&vars),
        env_steady: without_once(&vars),
        db_password,
    };
    Ok((prep, plan, manifest))
}

/// Step 11: the app may have died after `health` passed. Wait, then
/// `status`; when it is not active remove the route again so no traffic
/// reaches a dead site, and fail.
async fn verify(ctx: &JobCtx, site: &Site) -> Result<(), AppError> {
    if !ctx.verify_delay.is_zero() {
        tokio::time::sleep(ctx.verify_delay).await;
    }
    let status = ctx
        .agent_ok(
            &site.server_id,
            Command::Status {
                site: site.short_id.clone(),
            },
        )
        .await;
    let active = matches!(&status, Ok(out) if out.get("active").and_then(serde_json::Value::as_bool) == Some(true));
    if active {
        return Ok(());
    }
    let _ = ctx
        .agent_ok(
            &site.server_id,
            Command::RemoveRoute {
                site_short_id: site.short_id.clone(),
                domain: site.domain.clone(),
            },
        )
        .await;
    match status {
        Err(e) => Err(e),
        Ok(_) => Err(AppError::new(
            ErrorCode::Internal,
            "site is not active after routing",
        )),
    }
}
