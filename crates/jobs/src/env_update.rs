//! `env_update` (spec §6): the operator changed `prompt`/`value`
//! variables. Steps, resumed from the job's cursor: `validate` (every
//! changed name is a `value` or `prompt` rule of the site's manifest),
//! `env` (merge the changes into the stored env, seal and store),
//! `write_env` (without `once` values), `restart` (when `running`),
//! `finish`. The changes travel SEALED in the job payload (`PUT
//! /sites/{id}/env`); the runner opens them. An agent failure marks the
//! site `failed` with the step.

use std::collections::BTreeMap;

use setu_common::AppError;
use setu_manifest::EnvSpec;
use setu_proto::Command;

use crate::{
    load_manifest, mark_failed, register_secrets, sealed_env, site_env, without_once, JobCtx, Steps,
};

/// The steps, in order.
pub const STEPS: &[&str] = &["validate", "env", "write_env", "restart", "finish"];

/// Apply `changes` (plaintext name → value) to the site's env, rewrite
/// the env file and restart.
pub async fn run(
    ctx: &JobCtx,
    site_id: &str,
    changes: &BTreeMap<String, String>,
) -> Result<(), AppError> {
    for value in changes.values() {
        ctx.add_secret(value);
    }
    let site = ctx.db.sites().get(site_id).await?;
    let steps = Steps::run_from(ctx.cursor().await.as_deref(), STEPS);
    let manifest = load_manifest(&ctx.db, &site.app_id, &site.app_version).await?;

    steps
        .run(ctx, "validate", async {
            if site.status != "running" && site.status != "stopped" {
                return Err(AppError::validation(format!(
                    "site env cannot be updated from status {}",
                    site.status
                )));
            }
            for (name, value) in changes {
                match manifest.env.get(name) {
                    Some(EnvSpec::Value(_) | EnvSpec::Prompt(_)) => {}
                    Some(_) => {
                        return Err(AppError::validation(format!(
                            "env var {name} is generated and cannot be set"
                        )))
                    }
                    None => return Err(AppError::validation(format!("unknown env var {name}"))),
                }
                if !setu_common::validate::no_control(value) {
                    return Err(AppError::validation(format!(
                        "env var {name} must not contain control characters"
                    )));
                }
            }
            Ok(())
        })
        .await?;

    // Re-applying the same changes on resume is harmless.
    let stored = steps
        .run(ctx, "env", async {
            let mut vars = site_env(ctx, &site)?;
            for (name, value) in changes {
                let (secret, once) = match manifest.env.get(name) {
                    Some(EnvSpec::Prompt(p)) => (p.secret, p.once),
                    _ => (false, false),
                };
                match vars.iter_mut().find(|v| &v.name == name) {
                    Some(v) => v.value = value.clone(),
                    None => vars.push(setu_manifest::EnvVar {
                        name: name.clone(),
                        value: value.clone(),
                        secret,
                        once,
                    }),
                }
            }
            ctx.db
                .sites()
                .set_env(&site.id, &sealed_env(&ctx.sealer, &vars))
                .await?;
            Ok(vars)
        })
        .await
        .map_err(|e| ("env", e));
    let vars = match stored {
        Ok(Some(vars)) => vars,
        Ok(None) => site_env(ctx, &ctx.db.sites().get(site_id).await?)?,
        Err((name, e)) => return Err(mark_failed(ctx, site_id, name, e).await),
    };
    register_secrets(ctx, &vars);

    let short = site.short_id.clone();
    if let Err(e) = steps
        .run(
            ctx,
            "write_env",
            ctx.agent_ok(
                &site.server_id,
                Command::WriteEnv {
                    site: short.clone(),
                    env: without_once(&vars),
                },
            ),
        )
        .await
    {
        return Err(mark_failed(ctx, site_id, "write_env", e).await);
    }
    if site.status == "running" {
        if let Err(e) = steps
            .run(
                ctx,
                "restart",
                ctx.agent_ok(&site.server_id, Command::Restart { site: short }),
            )
            .await
        {
            return Err(mark_failed(ctx, site_id, "restart", e).await);
        }
    }
    steps.run(ctx, "finish", async { Ok(()) }).await?;
    Ok(())
}
