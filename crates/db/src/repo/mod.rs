pub mod apps;
pub mod audit;
pub mod backups;
pub mod commands;
pub mod jobs;
pub mod metrics;
pub mod operators;
pub mod plans;
pub mod servers;
pub mod sessions;
pub mod settings;
pub mod sites;

pub use apps::{App, AppVersion, Apps};
pub use audit::{Audit, AuditEntry};
pub use backups::{Backup, Backups};
pub use commands::{AgentCommand, Commands, WaitError};
pub use jobs::{Job, Jobs};
pub use metrics::{LatestMetric, Metrics};
pub use operators::{Operator, Operators};
pub use plans::{NewPlan, Plan, Plans};
pub use servers::{
    HeartbeatInput, HeartbeatOutcome, JoinError, JoinInput, Server, ServerStatus, Servers,
};
pub use sessions::{Session, Sessions};
pub use settings::Settings;
pub use sites::{CreateError, NewSite, Site, Sites};

/// `Duration` → seconds as `f64` for `make_interval(secs => $n)`.
pub(crate) fn secs(d: std::time::Duration) -> f64 {
    d.as_secs_f64()
}

/// 8 lowercase `[a-z0-9]` characters (servers' and sites' `short_id`).
pub(crate) fn random_short_id() -> String {
    use rand::Rng;
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::rng();
    (0..8)
        .map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char)
        .collect()
}

/// SHA-256 hex of a token's bytes (server, join and session tokens).
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(bytes))
}

/// 32 random bytes.
pub(crate) fn random_32() -> [u8; 32] {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    bytes
}
