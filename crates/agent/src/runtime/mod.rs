//! The `Runtime` trait (spec §1, exact shape) plus `NativeRuntime`
//! (binary + systemd service, phase 1).
//!
//! `NativeRuntime<S, P, C>` is generic over the systemd/Postgres/Caddy
//! adapters so tests run fully unprivileged behind the fakes; production
//! wires `SystemctlCli`/`PsqlCli`/`CaddyAdmin`.

mod native;

pub use native::NativeRuntime;
use setu_proto::SiteSpec;

/// Site id: `setu-site@<id>.service`, `setu-site-<id>` user.
pub type SiteId = String;

/// Unit state as observed by the runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeStatus {
    pub active: bool,
    pub sub: String,
    pub pid: Option<u32>,
}

/// Opaque log cursor (journal cursor in phase 2; line offset for now).
pub type Cursor = String;

/// One page of logs plus the cursor for the next page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogStream {
    pub lines: Vec<String>,
    pub next: Option<Cursor>,
}

/// Supervisor contract (spec §1). Phase 2 adds `QuadletRuntime` behind
/// this same trait; both produce a systemd unit.
#[async_trait::async_trait]
pub trait Runtime {
    async fn install(&self, site: &SiteSpec) -> anyhow::Result<()>;
    async fn write_unit(&self, site: &SiteSpec) -> anyhow::Result<()>;
    async fn start(&self, id: SiteId) -> anyhow::Result<()>;
    async fn stop(&self, id: SiteId) -> anyhow::Result<()>;
    async fn status(&self, id: SiteId) -> anyhow::Result<RuntimeStatus>;
    async fn logs(&self, id: SiteId, since: Option<Cursor>) -> anyhow::Result<LogStream>;
    async fn remove(&self, id: SiteId) -> anyhow::Result<()>;
}
