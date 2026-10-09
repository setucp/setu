//! The agent's fixed paths (spec §4), optionally under a test root.
//! Every site-derived path is checked against `^[a-z0-9]{8}$` first.

use std::path::PathBuf;

/// Agent paths. `root` is `/` in production.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    root: PathBuf,
}

impl Default for Paths {
    fn default() -> Self {
        Self::new(None)
    }
}

impl Paths {
    pub fn new(root: Option<PathBuf>) -> Self {
        Self {
            root: root.unwrap_or_else(|| PathBuf::from("/")),
        }
    }

    fn at(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// `/var/lib/setu`: the only tree the agent itself writes.
    pub fn var(&self) -> PathBuf {
        self.at("var/lib/setu")
    }

    pub fn state(&self) -> PathBuf {
        self.at("var/lib/setu/state.json")
    }

    pub fn downloads(&self) -> PathBuf {
        self.at("var/lib/setu/downloads")
    }

    pub fn backups(&self) -> PathBuf {
        self.at("var/lib/setu/backups")
    }

    /// `/srv/setu/sites` (root-owned; the agent only reads it).
    pub fn sites(&self) -> PathBuf {
        self.at("srv/setu/sites")
    }

    /// `/srv/setu/sites/<short>`.
    pub fn site_dir(&self, short: &str) -> anyhow::Result<PathBuf> {
        check_short(short)?;
        Ok(self.sites().join(short))
    }

    /// `/var/lib/setu/backups/<short>`.
    pub fn site_backups(&self, short: &str) -> anyhow::Result<PathBuf> {
        check_short(short)?;
        Ok(self.backups().join(short))
    }
}

/// `^[a-z0-9]{8}$` or an error naming nothing but the rule.
pub fn check_short(short: &str) -> anyhow::Result<()> {
    if setu_common::is_short_id(short) {
        Ok(())
    } else {
        anyhow::bail!("invalid site short id (must match ^[a-z0-9]{{8}}$)")
    }
}

/// Short id for a site id (ULID): its last 8 chars, lowercased, checked.
pub fn short_of(site_id: &str) -> anyhow::Result<String> {
    let short = setu_common::short_id(site_id).to_lowercase();
    check_short(&short)?;
    Ok(short)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_reject_bad_short_id() {
        let p = Paths::new(Some(PathBuf::from("/tmp/x")));
        assert_eq!(
            p.site_dir("abcd1234").expect("ok"),
            PathBuf::from("/tmp/x/srv/setu/sites/abcd1234")
        );
        for bad in [
            "",
            "ABCD1234",
            "abcd123",
            "abcd12345",
            "../abcde",
            "abcd/123",
            "abcd 123",
        ] {
            p.site_dir(bad).expect_err(bad);
            p.site_backups(bad).expect_err(bad);
        }
        assert_eq!(
            Paths::default().state(),
            PathBuf::from("/var/lib/setu/state.json")
        );
        assert_eq!(
            short_of("01jtestsite00000000000001").expect("ulid"),
            "00000001"
        );
        short_of("01JTESTSITE0000000000AB/").expect_err("bad tail");
    }
}
