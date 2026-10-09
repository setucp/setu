//! `setu helper <verb> --site <short>`: stdin is one line of JSON (the
//! verb input), followed by the archive for `restore`; stdout is one
//! line of JSON (the result), or the archive for `backup`. Errors go to
//! stderr with exit status 1.
//!
//! `--root <dir>` exists only with the cargo feature `test-root` and is
//! refused when `SUDO_UID` is set, so sudo can never reach it.

use std::io::{BufRead, Read, Write};

use super::{run, validate, Ctx, HelperError, Verb};

/// Largest accepted JSON input line (env files are the big ones).
const MAX_INPUT: u64 = 1 << 20;

/// Parsed `setu helper` arguments.
#[derive(Debug, Clone, clap::Args)]
pub struct HelperArgs {
    /// The verb (spec §4).
    #[arg(value_enum)]
    pub verb: Verb,
    /// Site short id (`^[a-z0-9]{8}$`).
    #[arg(long)]
    pub site: String,
    /// Test-only filesystem root (feature `test-root`; refused under sudo).
    #[cfg(feature = "test-root")]
    #[arg(long)]
    pub root: Option<std::path::PathBuf>,
}

/// Process entry: real stdin/stdout, `SUDO_UID` from the environment.
/// Returns the exit status.
pub fn main(args: HelperArgs) -> i32 {
    let under_sudo = std::env::var_os("SUDO_UID").is_some();
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let verb = args.verb;
    match run_cli(args, under_sudo, &mut stdin.lock(), &mut stdout.lock()) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("setu helper {verb}: {e}");
            1
        }
    }
}

/// Build the context for `args`: production, or (feature `test-root`)
/// an unprivileged fake context at `--root`, never under sudo.
fn context(args: &HelperArgs, under_sudo: bool) -> Result<Ctx, HelperError> {
    #[cfg(feature = "test-root")]
    if let Some(root) = &args.root {
        if under_sudo {
            return Err(HelperError::Refused(
                "--root is not allowed under sudo".into(),
            ));
        }
        return Ok(Ctx::fake(root));
    }
    let _ = (args, under_sudo);
    Ok(Ctx::production())
}

/// Testable body of [`main`].
pub fn run_cli(
    args: HelperArgs,
    under_sudo: bool,
    stdin: &mut dyn BufRead,
    stdout: &mut dyn Write,
) -> Result<(), HelperError> {
    let ctx = context(&args, under_sudo)?;
    validate::site(&args.site)?;
    let mut line = Vec::new();
    (&mut *stdin)
        .take(MAX_INPUT)
        .read_until(b'\n', &mut line)
        .map_err(|e| HelperError::io("stdin", e))?;
    if line.last() != Some(&b'\n') {
        return Err(HelperError::Invalid(
            "input (one JSON line on stdin)".into(),
        ));
    }
    let input: serde_json::Value =
        serde_json::from_slice(&line).map_err(|_| HelperError::Invalid("input json".into()))?;
    let out = run(&ctx, args.verb, &args.site, input, stdin, stdout)?;
    if args.verb != Verb::Backup {
        let mut text = serde_json::to_vec(&out).map_err(|e| HelperError::io("stdout", e))?;
        text.push(b'\n');
        stdout
            .write_all(&text)
            .and_then(|()| stdout.flush())
            .map_err(|e| HelperError::io("stdout", e))?;
    }
    Ok(())
}
