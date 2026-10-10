//! Child processes whose bulk data streams between files and pipes
//! (I3): backups and restores never hold an archive or a dump in agent
//! memory. Used by [`crate::helper_client::SudoHelper`] (helper `backup`
//! / `restore`) and [`crate::postgres::TokioRunner`] (`pg_dump` /
//! `pg_restore`).

use std::path::Path;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// Most stderr kept for the error line; the rest is drained.
const STDERR_CAP: u64 = 64 * 1024;

/// What a streamed process returned.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Streamed {
    pub success: bool,
    /// Captured stdout; empty when it was streamed to a file.
    pub stdout: Vec<u8>,
    /// The first [`STDERR_CAP`] bytes of stderr.
    pub stderr: Vec<u8>,
    /// Bytes written to the `stdout_to` file (0 without one).
    pub streamed: u64,
}

/// Create `path` (0600, truncating) for a streamed stdout.
pub async fn create_private(path: &Path) -> std::io::Result<tokio::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    Ok(tokio::fs::File::from_std(file))
}

/// Run `argv`: write `stdin_prefix`, then (when given) the file
/// `stdin_from` to its stdin with `tokio::io::copy`; copy its stdout into
/// the file `stdout_to` (synced) or capture it. Input and output run
/// concurrently, so neither pipe can deadlock the other. A missing
/// `stdin_from` fails before the process starts.
pub async fn run(
    argv: &[String],
    stdin_prefix: Vec<u8>,
    stdin_from: Option<&Path>,
    stdout_to: Option<&Path>,
) -> anyhow::Result<Streamed> {
    let (prog, args) = argv
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("empty argv"))?;
    let input = match stdin_from {
        Some(path) => Some(
            tokio::fs::File::open(path)
                .await
                .map_err(|e| anyhow::anyhow!("cannot open {}: {e}", path.display()))?,
        ),
        None => None,
    };
    let output = match stdout_to {
        Some(path) => Some(
            create_private(path)
                .await
                .map_err(|e| anyhow::anyhow!("cannot create {}: {e}", path.display()))?,
        ),
        None => None,
    };
    let mut child = tokio::process::Command::new(prog)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| anyhow::anyhow!("cannot run {prog}: {e}"))?;
    let mut pipe = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("{prog}: no stdin pipe"))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("{prog}: no stdout pipe"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("{prog}: no stderr pipe"))?;

    let write_in = async move {
        pipe.write_all(&stdin_prefix).await?;
        if let Some(mut input) = input {
            tokio::io::copy(&mut input, &mut pipe).await?;
        }
        pipe.shutdown().await?;
        drop(pipe);
        Ok::<(), std::io::Error>(())
    };
    let read_out = async {
        match output {
            Some(mut file) => {
                let n = tokio::io::copy(&mut stdout, &mut file).await?;
                file.flush().await?;
                file.sync_all().await?;
                Ok::<_, std::io::Error>((Vec::new(), n))
            }
            None => {
                let mut buf = Vec::new();
                stdout.read_to_end(&mut buf).await?;
                Ok((buf, 0))
            }
        }
    };
    let read_err = async {
        let mut buf = Vec::new();
        (&mut stderr).take(STDERR_CAP).read_to_end(&mut buf).await?;
        tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await?;
        Ok::<_, std::io::Error>(buf)
    };
    let (written, out, err) = tokio::join!(write_in, read_out, read_err);
    let status = child
        .wait()
        .await
        .map_err(|e| anyhow::anyhow!("{prog} failed to finish: {e}"))?;
    let (stdout, streamed) = out.map_err(|e| anyhow::anyhow!("{prog}: reading stdout: {e}"))?;
    let stderr = err.unwrap_or_default();
    // A child that exits early closes its stdin: a broken pipe then is
    // not the interesting error, the exit status is.
    if let Err(e) = written {
        if status.success() && e.kind() != std::io::ErrorKind::BrokenPipe {
            anyhow::bail!("{prog}: writing stdin: {e}");
        }
    }
    Ok(Streamed {
        success: status.success(),
        stdout,
        stderr,
        streamed,
    })
}
