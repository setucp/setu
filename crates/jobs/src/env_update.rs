//! `env_update` (spec §6, 0.1.4 §1): the operator changed `prompt`/`value`
//! variables, or added or removed extra variables (names the manifest
//! does not declare). Steps, resumed from the job's cursor: `validate`
//! (every changed name is a `value` or `prompt` rule of the site's
//! manifest; every extra name is valid, unreserved and undeclared; every
//! removed name is an existing extra), `env` (merge the changes into the
//! stored env, seal and store), `write_env` (without `once` values),
//! `restart` (when `running`), `finish`. The values travel SEALED in the
//! job payload (`PUT /sites/{id}/env`); the runner opens them. An agent
//! failure marks the site `failed` with the step.

use std::collections::BTreeMap;

use setu_common::AppError;
use setu_manifest::EnvSpec;
use setu_proto::Command;

use crate::{
    load_manifest, mark_failed, register_secrets, sealed_env, site_env, without_once, JobCtx, Steps,
};

/// The steps, in order.
pub const STEPS: &[&str] = &["validate", "env", "write_env", "restart", "finish"];

/// What one `env_update` changes, in plaintext.
#[derive(Default)]
pub struct EnvChanges {
    /// Manifest `value`/`prompt` variables: name → value.
    pub manifest: BTreeMap<String, String>,
    /// Extra variables to add or replace: name → (value, secret).
    pub extra: BTreeMap<String, (String, bool)>,
    /// Extra variables to delete.
    pub remove: Vec<String>,
}

// Holds env values: `Debug` prints their names only.
impl std::fmt::Debug for EnvChanges {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnvChanges")
            .field("manifest", &self.manifest.keys().collect::<Vec<_>>())
            .field("extra", &self.extra.keys().collect::<Vec<_>>())
            .field("remove", &self.remove)
            .finish()
    }
}

/// Checks `changes` against the site's manifest and stored env: the
/// same rules `PUT /sites/{id}/env` applies before enqueueing.
pub fn validate(
    manifest: &setu_manifest::Manifest,
    stored: &[setu_manifest::EnvVar],
    changes: &EnvChanges,
) -> Result<(), AppError> {
    for (name, value) in &changes.manifest {
        match manifest.env.get(name) {
            Some(EnvSpec::Value(_) | EnvSpec::Prompt(_)) => {}
            Some(_) => {
                return Err(AppError::validation(format!(
                    "env var {name} is generated and cannot be set"
                )))
            }
            None => return Err(AppError::validation(format!("unknown env var {name}"))),
        }
        no_control(name, value)?;
    }
    for (name, (value, _)) in &changes.extra {
        setu_common::validate::extra_env_name(name).map_err(AppError::validation)?;
        if manifest.env.contains_key(name) {
            return Err(AppError::validation(format!(
                "env var {name} is declared by the manifest; set it as a manifest variable"
            )));
        }
        no_control(name, value)?;
    }
    for name in &changes.remove {
        let is_extra = !manifest.env.contains_key(name) && stored.iter().any(|v| &v.name == name);
        if !is_extra {
            return Err(AppError::validation(format!(
                "env var {name} is not an extra variable of this site"
            )));
        }
    }
    let extras_after = stored
        .iter()
        .filter(|v| !manifest.env.contains_key(&v.name))
        .map(|v| v.name.as_str())
        .chain(changes.extra.keys().map(String::as_str))
        .filter(|n| !changes.remove.iter().any(|r| r == n))
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    if extras_after > setu_common::validate::MAX_EXTRA_ENV {
        return Err(AppError::validation(format!(
            "a site may have at most {} extra env vars",
            setu_common::validate::MAX_EXTRA_ENV
        )));
    }
    Ok(())
}

fn no_control(name: &str, value: &str) -> Result<(), AppError> {
    if setu_common::validate::no_control(value) {
        Ok(())
    } else {
        Err(AppError::validation(format!(
            "env var {name} must not contain control characters"
        )))
    }
}

/// Applies `changes` to `vars` in place: manifest values, then extras,
/// then removals.
fn apply(
    manifest: &setu_manifest::Manifest,
    vars: &mut Vec<setu_manifest::EnvVar>,
    changes: &EnvChanges,
) {
    let mut set = |name: &str, value: &str, secret: bool, once: bool| match vars
        .iter_mut()
        .find(|v| v.name == name)
    {
        Some(v) => {
            v.value = value.to_string();
            v.secret = secret;
        }
        None => vars.push(setu_manifest::EnvVar {
            name: name.to_string(),
            value: value.to_string(),
            secret,
            once,
        }),
    };
    for (name, value) in &changes.manifest {
        let (secret, once) = match manifest.env.get(name) {
            Some(EnvSpec::Prompt(p)) => (p.secret, p.once),
            _ => (false, false),
        };
        set(name, value, secret, once);
    }
    for (name, (value, secret)) in &changes.extra {
        set(name, value, *secret, false);
    }
    vars.retain(|v| !changes.remove.contains(&v.name));
}

/// Apply `changes` to the site's env, rewrite the env file and restart.
pub async fn run(ctx: &JobCtx, site_id: &str, changes: &EnvChanges) -> Result<(), AppError> {
    for value in changes.manifest.values() {
        ctx.add_secret(value);
    }
    for (value, _) in changes.extra.values() {
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
            validate(&manifest, &site_env(ctx, &site)?, changes)
        })
        .await?;

    // Re-applying the same changes on resume is harmless.
    let stored = steps
        .run(ctx, "env", async {
            let mut vars = site_env(ctx, &site)?;
            apply(&manifest, &mut vars, changes);
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
