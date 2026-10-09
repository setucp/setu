//! The `Runtime` trait (spec §1, exact shape) plus `NativeRuntime`
//! (binary + systemd service, phase 1).
//!
//! `NativeRuntime<H, P, C>` is generic over the privileged helper and the
//! Postgres/Caddy adapters so tests run fully unprivileged behind the
//! fakes; production wires `SudoHelper`/`PsqlSudo`/`CaddySocket`.

mod native;

pub use native::NativeRuntime;
use setu_proto::SiteSpec;

/// Site id: `setu-site@<id>.service`, `setu-site-<id>` user.
pub type SiteId = String;

/// Unit state as observed by the runtime (helper `unit show`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeStatus {
    pub active: bool,
    pub sub: String,
    pub pid: Option<u32>,
    /// `ControlGroup` (for [`crate::metrics::cgroup_path`]).
    pub control_group: Option<String>,
    /// `MemoryCurrent` bytes, when systemd tracks it.
    pub memory_bytes: Option<u64>,
    /// `CPUUsageNSec`, when systemd tracks it.
    pub cpu_nsec: Option<u64>,
    pub n_restarts: u32,
    /// `du -sb data/` (helper, as root).
    pub data_bytes: u64,
}

impl RuntimeStatus {
    /// Map the helper's `unit show` JSON.
    pub fn from_show(v: &serde_json::Value) -> Self {
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        let n = |k: &str| {
            v.get(k)
                .and_then(|x| x.as_str())
                .and_then(|x| x.parse::<u64>().ok())
        };
        let state = s("ActiveState");
        let cg = s("ControlGroup");
        Self {
            active: state == "active",
            sub: s("SubState"),
            pid: n("MainPID")
                .filter(|p| *p != 0)
                .and_then(|p| u32::try_from(p).ok()),
            control_group: (!cg.is_empty()).then_some(cg),
            memory_bytes: n("MemoryCurrent"),
            cpu_nsec: n("CPUUsageNSec"),
            n_restarts: n("NRestarts")
                .and_then(|x| u32::try_from(x).ok())
                .unwrap_or(0),
            data_bytes: v.get("data_bytes").and_then(|x| x.as_u64()).unwrap_or(0),
        }
    }
}

/// Opaque log cursor (the journal `__CURSOR`).
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
