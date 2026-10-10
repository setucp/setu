//! Backups stream: the helper's `backup` stdout and `pg_dump`'s stdout go
//! to a file, the `restore` archive and `pg_restore`'s input come from a
//! file — never a whole buffer in the agent (I3). One test in its own
//! binary, so the process's peak RSS (`VmHWM`) measures only this test.

use setu_agent::helper::Verb;
use setu_agent::helper_client::{Helper, HelperIo, SudoHelper};
use setu_agent::postgres::{CommandRunner, TokioRunner};

const SIZE: u64 = 100 * 1024 * 1024;
const MAX_GROWTH: u64 = 32 * 1024 * 1024;

/// Peak resident set of this process so far, in bytes.
fn peak_rss() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").expect("proc status");
    let kb: u64 = status
        .lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
        .expect("VmHWM");
    kb * 1024
}

fn sh(script: &str) -> SudoHelper {
    SudoHelper::with_program(
        "/bin/sh".into(),
        vec!["-c".into(), script.into(), "sh".into()],
        Vec::new(),
    )
}

#[tokio::test]
async fn backup_and_restore_stream_without_buffering() {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = peak_rss();

    // Helper `backup`: 100 MB on stdout land in the file.
    let archive = dir.path().join("dirs.tar.zst");
    let out = sh("head -c 104857600 /dev/zero")
        .call(
            Verb::Backup,
            "abcd1234",
            serde_json::json!({"dirs": ["media"]}),
            HelperIo::stdout_to(&archive),
        )
        .await
        .expect("backup streams");
    assert_eq!(out.streamed, SIZE);
    assert_eq!(std::fs::metadata(&archive).expect("archive").len(), SIZE);

    // Helper `restore`: the JSON line, then 100 MB from the file on stdin.
    let sparse = dir.path().join("big.tar.zst");
    std::fs::File::create(&sparse)
        .expect("create")
        .set_len(SIZE)
        .expect("sparse");
    let out = sh("IFS= read -r line; n=$(wc -c); echo \"{\\\"bytes\\\": $n}\"")
        .call(
            Verb::Restore,
            "abcd1234",
            serde_json::json!({"dirs": ["media"]}),
            HelperIo::stdin_from(&sparse),
        )
        .await
        .expect("restore streams");
    assert_eq!(out.json["bytes"], SIZE);

    // `pg_dump` / `pg_restore` through the runner, same shape.
    let dump = dir.path().join("db.dump");
    let run = TokioRunner
        .run_io(
            &[
                "/bin/sh".into(),
                "-c".into(),
                "head -c 104857600 /dev/zero".into(),
            ],
            None,
            Some(&dump),
        )
        .await
        .expect("dump streams");
    assert!(run.success);
    assert!(run.stdout.is_empty(), "streamed stdout is not buffered");
    assert_eq!(std::fs::metadata(&dump).expect("dump").len(), SIZE);
    let run = TokioRunner
        .run_io(
            &["/bin/sh".into(), "-c".into(), "wc -c".into()],
            Some(&sparse),
            None,
        )
        .await
        .expect("restore streams");
    assert!(run.success);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        SIZE.to_string()
    );

    let growth = peak_rss().saturating_sub(base);
    assert!(
        growth < MAX_GROWTH,
        "peak RSS grew by {} MB moving 4 × 100 MB",
        growth / (1024 * 1024)
    );
}
