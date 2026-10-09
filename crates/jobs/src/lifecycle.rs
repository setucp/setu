//! `start_site`, `stop_site`, `restart_site` (spec §6): jobs, so they
//! serialise with the site's other work. One agent step each, then the
//! status (`running` or `stopped`); an agent failure marks the site
//! `failed` with the step. Repeating an operation is an idempotent
//! success (the agent call is still issued: the unit may have changed
//! out of band).

use setu_common::AppError;
use setu_proto::Command;

use crate::{mark_failed, JobCtx};

/// One lifecycle operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Start,
    Stop,
    Restart,
}

impl Op {
    /// The operation of a job kind (`start_site` …).
    pub fn from_job_kind(kind: &str) -> Option<Self> {
        match kind {
            "start_site" => Some(Self::Start),
            "stop_site" => Some(Self::Stop),
            "restart_site" => Some(Self::Restart),
            _ => None,
        }
    }

    fn step(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
        }
    }
}

/// Run `op` on the site.
pub async fn run(ctx: &JobCtx, site_id: &str, op: Op) -> Result<(), AppError> {
    let site = ctx.db.sites().get(site_id).await?;
    if site.status != "running" && site.status != "stopped" {
        return Err(AppError::validation(format!(
            "site {site_id} cannot {} from status {}",
            op.step(),
            site.status
        )));
    }
    let short = site.short_id.clone();
    let cmd = match op {
        Op::Start => Command::Start { site: short },
        Op::Stop => Command::Stop { site: short },
        Op::Restart => Command::Restart { site: short },
    };
    if let Err(e) = ctx
        .step(op.step(), ctx.agent_ok(&site.server_id, cmd))
        .await
    {
        return Err(mark_failed(ctx, site_id, op.step(), e).await);
    }
    let status = if op == Op::Stop { "stopped" } else { "running" };
    ctx.step("finish", async {
        ctx.db.sites().set_status(site_id, status, None).await?;
        Ok(())
    })
    .await
}
