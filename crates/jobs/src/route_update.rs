//! `route_update` (0.1.7): the site's Caddy route is rewritten from the
//! stored basic-auth credentials (set, replaced or removed through
//! `PUT`/`DELETE /sites/{id}/basic-auth`). One agent step, `set_route`,
//! carrying the stored user name and bcrypt hash (never a password). The
//! site keeps its status: a failed route change leaves the old route in
//! place and fails only the job, and `GET /sites/{id}/basic-auth` reports
//! it (`route.status = failed`, read from this job) until a later change
//! applies.

use setu_common::{AppError, ErrorCode};
use setu_proto::{Alias, AliasMode, BasicAuth, Command};

use crate::JobCtx;

/// The `SetRoute` command for a site from its stored credentials.
pub async fn route_command(ctx: &JobCtx, site_id: &str) -> Result<Command, AppError> {
    let site = ctx.db.sites().get(site_id).await?;
    let auth = ctx.db.basic_auths().get(site_id).await?;
    let aliases = ctx.db.aliases().list(site_id).await?;
    let noindex = ctx.db.options().noindex(site_id).await?;
    let port = u16::try_from(site.port)
        .map_err(|_| AppError::validation(format!("site {site_id} has an invalid port")))?;
    Ok(Command::SetRoute {
        site_short_id: site.short_id,
        domain: site.domain,
        port,
        basic_auth: auth.map(|a| BasicAuth {
            username: a.username,
            password_hash: a.password_hash,
        }),
        www_redirect: site.www_redirect,
        aliases: aliases
            .into_iter()
            .map(|a| {
                let mode = match a.mode.as_str() {
                    "serve" => AliasMode::Serve,
                    "redirect" => AliasMode::Redirect,
                    other => {
                        return Err(AppError::validation(format!(
                            "alias {} has unknown mode {other}",
                            a.host
                        )))
                    }
                };
                Ok(Alias { host: a.host, mode })
            })
            .collect::<Result<_, AppError>>()?,
        noindex,
    })
}

/// The job fails unless the agent's answer shows it applied what was sent:
/// an agent older than 0.1.7 ignores `aliases` and `noindex` and answers
/// ok; one that dropped an alias (another site lists the same host) says
/// fewer than were sent. `output` is the agent's `set_route` result.
fn check_applied(
    sent_aliases: usize,
    sent_noindex: bool,
    output: &serde_json::Value,
) -> Result<(), AppError> {
    if sent_aliases == 0 && !sent_noindex {
        // Nothing the 0.1.7 agent added was sent (a www-redirect or basic-auth
        // change): an older agent applies it in full and echoes nothing.
        return Ok(());
    }
    let live = output.get("aliases").and_then(serde_json::Value::as_u64);
    let noindex = output.get("noindex").and_then(serde_json::Value::as_bool);
    if noindex != Some(sent_noindex) {
        return Err(AppError::new(
            ErrorCode::Internal,
            "the agent did not confirm the noindex setting (is it older than 0.1.7?)",
        ));
    }
    match live {
        Some(n) if usize::try_from(n) == Ok(sent_aliases) => Ok(()),
        Some(n) => Err(AppError::new(
            ErrorCode::Internal,
            format!(
                "the agent routes {n} of {sent_aliases} aliases: \
             another site on the server lists the same host"
            ),
        )),
        None => Err(AppError::new(
            ErrorCode::Internal,
            "the agent did not confirm the aliases (is it older than 0.1.7?)",
        )),
    }
}

/// Run the job for `site_id`.
pub async fn run(ctx: &JobCtx, site_id: &str) -> Result<(), AppError> {
    let site = ctx.db.sites().get(site_id).await?;
    if site.status != "running" && site.status != "stopped" {
        return Err(AppError::validation(format!(
            "site {site_id} cannot change its route from status {}",
            site.status
        )));
    }
    ctx.step("set_route", async {
        let cmd = route_command(ctx, site_id).await?;
        let (n, noindex) = match &cmd {
            Command::SetRoute {
                aliases, noindex, ..
            } => (aliases.len(), *noindex),
            _ => (0, false),
        };
        let output = ctx.agent_ok(&site.server_id, cmd).await?;
        check_applied(n, noindex, &output)
    })
    .await?;
    ctx.step("finish", async { Ok(()) }).await
}

#[cfg(test)]
mod tests {
    use super::check_applied;
    use serde_json::json;

    #[test]
    fn an_old_agent_that_echoes_nothing_fails_the_job() {
        let old = json!({"domain": "a.example.com", "route_id": "x", "www": null});
        assert!(check_applied(1, false, &old).is_err());
        assert!(check_applied(0, true, &old).is_err());
        // a www-redirect or basic-auth job sends neither: an old agent is fine
        assert!(check_applied(0, false, &old).is_ok());
    }

    #[test]
    fn a_dropped_alias_fails_the_job() {
        let out = json!({"aliases": 1, "noindex": false});
        assert!(check_applied(2, false, &out).is_err());
    }

    #[test]
    fn a_full_echo_passes() {
        let out = json!({"aliases": 2, "noindex": true});
        assert!(check_applied(2, true, &out).is_ok());
    }
}
