//! The agent's side of the sudo boundary: [`Helper`] runs one helper
//! verb and returns its JSON result; `backup` streams its archive into a
//! file and `restore` streams one from a file ([`HelperIo`]), so an
//! archive never sits in agent memory (I3).
//!
//! [`SudoHelper`] runs exactly `sudo -n /opt/setu/setu helper <verb>
//! --site <short>` (`-n`: never prompt, so a missing sudoers rule fails
//! fast instead of hanging the agent). [`FakeHelper`] records calls and
//! returns scripted outputs; [`LocalHelper`] runs the real helper code
//! in-process against a fake [`Ctx`] (tests).

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::helper::{Ctx, Verb};

/// The installed binary sudoers pins (root:root 0755, spec §4).
pub const HELPER_PATH: &str = "/opt/setu/setu";

/// One helper result.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HelperOutput {
    /// The JSON result (`Null` for `backup`).
    pub json: serde_json::Value,
    /// Raw stdout when it was not streamed to a file (the JSON text).
    pub stdout: Vec<u8>,
    /// Bytes written to [`HelperIo::stdout_to`] (0 without it).
    pub streamed: u64,
}

/// Where a streaming verb's bulk bytes come from and go to: the agent's
/// own files under `/var/lib/setu/backups`. Paths stay on the agent's
/// side; only the bytes cross the sudo boundary (stdin/stdout).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HelperIo {
    /// `restore`: streamed to the helper's stdin after the JSON line.
    pub stdin_from: Option<PathBuf>,
    /// `backup`: the helper's stdout is streamed into this file.
    pub stdout_to: Option<PathBuf>,
}

impl HelperIo {
    /// No bulk data: the JSON line in, the JSON result out.
    pub fn none() -> Self {
        Self::default()
    }

    pub fn stdin_from(path: &Path) -> Self {
        Self {
            stdin_from: Some(path.to_path_buf()),
            stdout_to: None,
        }
    }

    pub fn stdout_to(path: &Path) -> Self {
        Self {
            stdin_from: None,
            stdout_to: Some(path.to_path_buf()),
        }
    }
}

/// Run one privileged verb for one site.
#[async_trait::async_trait]
pub trait Helper: Send + Sync {
    async fn call(
        &self,
        verb: Verb,
        short: &str,
        input: serde_json::Value,
        io: HelperIo,
    ) -> anyhow::Result<HelperOutput>;
}

/// stdin framing: the JSON input on one line, then the raw bytes.
pub fn frame_stdin(input: &serde_json::Value, extra: Option<&[u8]>) -> anyhow::Result<Vec<u8>> {
    let mut buf = serde_json::to_vec(input)?;
    buf.push(b'\n');
    if let Some(extra) = extra {
        buf.extend_from_slice(extra);
    }
    Ok(buf)
}

/// Parse helper stdout for `verb` (empty for a streamed `backup`).
pub fn parse_stdout(verb: Verb, stdout: Vec<u8>) -> anyhow::Result<HelperOutput> {
    if verb == Verb::Backup {
        return Ok(HelperOutput {
            json: serde_json::Value::Null,
            stdout,
            streamed: 0,
        });
    }
    let json = if stdout.iter().all(u8::is_ascii_whitespace) {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&stdout)
            .map_err(|e| anyhow::anyhow!("helper {verb}: bad output: {e}"))?
    };
    Ok(HelperOutput {
        json,
        stdout,
        streamed: 0,
    })
}

/// Production helper: `sudo -n /opt/setu/setu helper <verb> --site <short>`.
#[derive(Debug, Clone, Default)]
pub struct SudoHelper {
    /// Test-only override: `(program, args before the verb, args after)`.
    #[cfg(feature = "test-root")]
    program: Option<(std::path::PathBuf, Vec<String>, Vec<String>)>,
}

impl SudoHelper {
    pub fn new() -> Self {
        Self::default()
    }

    /// Run `program <pre…> <verb> --site <short> <post…>` instead of sudo
    /// (feature `test-root` only; e.g. `setu helper … --root <dir>`).
    #[cfg(feature = "test-root")]
    pub fn with_program(program: std::path::PathBuf, pre: Vec<String>, post: Vec<String>) -> Self {
        Self {
            program: Some((program, pre, post)),
        }
    }

    /// The exact argv (program first).
    pub fn argv(&self, verb: Verb, short: &str) -> Vec<String> {
        #[cfg(feature = "test-root")]
        if let Some((program, pre, post)) = &self.program {
            let mut v = vec![program.display().to_string()];
            v.extend(pre.iter().cloned());
            v.extend([
                verb.as_str().to_string(),
                "--site".into(),
                short.to_string(),
            ]);
            v.extend(post.iter().cloned());
            return v;
        }
        [
            "sudo",
            "-n",
            HELPER_PATH,
            "helper",
            verb.as_str(),
            "--site",
            short,
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }
}

#[async_trait::async_trait]
impl Helper for SudoHelper {
    async fn call(
        &self,
        verb: Verb,
        short: &str,
        input: serde_json::Value,
        io: HelperIo,
    ) -> anyhow::Result<HelperOutput> {
        if !setu_common::is_short_id(short) {
            anyhow::bail!("helper {verb}: invalid site id");
        }
        let argv = self.argv(verb, short);
        let prefix = frame_stdin(&input, None)?;
        let out = crate::stream::run(
            &argv,
            prefix,
            io.stdin_from.as_deref(),
            io.stdout_to.as_deref(),
        )
        .await
        .map_err(|e| anyhow::anyhow!("helper {verb} failed: {e}"))?;
        if !out.success {
            let err = String::from_utf8_lossy(&out.stderr);
            anyhow::bail!(
                "helper {verb} failed: {}",
                err.lines().next().unwrap_or("non-zero exit")
            );
        }
        let mut parsed = parse_stdout(verb, out.stdout)?;
        parsed.streamed = out.streamed;
        Ok(parsed)
    }
}

/// One recorded call. `Debug` hides inputs (write-env carries values).
#[derive(Clone, PartialEq)]
pub struct HelperCall {
    pub verb: Verb,
    pub short: String,
    pub input: serde_json::Value,
    pub stdin: Option<Vec<u8>>,
}

impl std::fmt::Debug for HelperCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HelperCall")
            .field("verb", &self.verb)
            .field("short", &self.short)
            .field("input", &"[redacted]")
            .field("stdin_len", &self.stdin.as_ref().map(Vec::len))
            .finish()
    }
}

#[derive(Debug, Default)]
struct FakeInner {
    calls: Vec<HelperCall>,
    scripted: HashMap<Verb, VecDeque<Result<HelperOutput, String>>>,
    delays: HashMap<Verb, std::time::Duration>,
}

/// Test double: records calls; returns scripted outputs per verb, else
/// a plausible default (`{"changed": true}`, an inactive `show`, …).
#[derive(Debug, Clone, Default)]
pub struct FakeHelper {
    inner: Arc<Mutex<FakeInner>>,
}

impl FakeHelper {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FakeInner> {
        self.inner.lock().expect("fake helper lock")
    }

    /// Queue a JSON result for the next `verb` call.
    pub fn push_json(&self, verb: Verb, json: serde_json::Value) {
        let stdout = serde_json::to_vec(&json).unwrap_or_default();
        self.lock()
            .scripted
            .entry(verb)
            .or_default()
            .push_back(Ok(HelperOutput {
                json,
                stdout,
                streamed: 0,
            }));
    }

    /// Queue raw stdout (an archive) for the next `verb` call.
    pub fn push_stdout(&self, verb: Verb, stdout: Vec<u8>) {
        self.lock()
            .scripted
            .entry(verb)
            .or_default()
            .push_back(Ok(HelperOutput {
                json: serde_json::Value::Null,
                stdout,
                streamed: 0,
            }));
    }

    /// Queue a failure for the next `verb` call.
    pub fn push_error(&self, verb: Verb, msg: &str) {
        self.lock()
            .scripted
            .entry(verb)
            .or_default()
            .push_back(Err(msg.to_string()));
    }

    /// Every later `verb` call sleeps `delay` (after being recorded).
    pub fn set_delay(&self, verb: Verb, delay: std::time::Duration) {
        self.lock().delays.insert(verb, delay);
    }

    pub fn calls(&self) -> Vec<HelperCall> {
        self.lock().calls.clone()
    }

    /// Verbs called, in order.
    pub fn verbs(&self) -> Vec<Verb> {
        self.lock().calls.iter().map(|c| c.verb).collect()
    }

    pub fn calls_for(&self, verb: Verb) -> Vec<HelperCall> {
        self.lock()
            .calls
            .iter()
            .filter(|c| c.verb == verb)
            .cloned()
            .collect()
    }
}

fn default_output(verb: Verb, input: &serde_json::Value) -> serde_json::Value {
    match verb {
        Verb::WriteUnit | Verb::Install | Verb::SwitchVersion | Verb::WriteEnv => {
            serde_json::json!({ "changed": true })
        }
        Verb::Unit if input["op"] == "show" => serde_json::json!({
            "ActiveState": "inactive", "SubState": "dead", "MainPID": "0",
            "ControlGroup": "", "MemoryCurrent": "[not set]", "CPUUsageNSec": "[not set]",
            "NRestarts": "0", "data_bytes": 0
        }),
        Verb::Backup => serde_json::Value::Null,
        _ => serde_json::json!({}),
    }
}

/// Deliver a scripted or default `out` through `io` the way the real
/// helper would: stdout into the `stdout_to` file.
fn deliver(io: &HelperIo, mut out: HelperOutput) -> anyhow::Result<HelperOutput> {
    if let Some(path) = &io.stdout_to {
        std::fs::write(path, &out.stdout)
            .map_err(|e| anyhow::anyhow!("cannot write {}: {e}", path.display()))?;
        out.streamed = out.stdout.len() as u64;
        out.stdout = Vec::new();
    }
    Ok(out)
}

#[async_trait::async_trait]
impl Helper for FakeHelper {
    async fn call(
        &self,
        verb: Verb,
        short: &str,
        input: serde_json::Value,
        io: HelperIo,
    ) -> anyhow::Result<HelperOutput> {
        // The fake records what would have been streamed in (tests only;
        // the inputs are small).
        let stdin = match &io.stdin_from {
            Some(path) => Some(
                std::fs::read(path)
                    .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", path.display()))?,
            ),
            None => None,
        };
        let (scripted, delay) = {
            let mut inner = self.lock();
            inner.calls.push(HelperCall {
                verb,
                short: short.to_string(),
                input: input.clone(),
                stdin,
            });
            (
                inner.scripted.get_mut(&verb).and_then(VecDeque::pop_front),
                inner.delays.get(&verb).copied(),
            )
        };
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        match scripted {
            Some(Ok(out)) => deliver(&io, out),
            Some(Err(msg)) => Err(anyhow::anyhow!("helper {verb} failed: {msg}")),
            None => {
                let json = default_output(verb, &input);
                let out = HelperOutput {
                    stdout: if verb == Verb::Backup {
                        Vec::new()
                    } else {
                        serde_json::to_vec(&json).unwrap_or_default()
                    },
                    json,
                    streamed: 0,
                };
                deliver(&io, out)
            }
        }
    }
}

/// Runs the real helper code in-process against `ctx` (a tempdir root
/// with fakes). Tests only: production always crosses sudo.
#[derive(Debug, Clone)]
pub struct LocalHelper {
    ctx: Arc<Ctx>,
}

impl LocalHelper {
    pub fn new(ctx: Ctx) -> Self {
        Self { ctx: Arc::new(ctx) }
    }

    pub fn ctx(&self) -> &Ctx {
        &self.ctx
    }
}

#[async_trait::async_trait]
impl Helper for LocalHelper {
    async fn call(
        &self,
        verb: Verb,
        short: &str,
        input: serde_json::Value,
        io: HelperIo,
    ) -> anyhow::Result<HelperOutput> {
        let ctx = self.ctx.clone();
        let short = short.to_string();
        tokio::task::spawn_blocking(move || {
            let mut reader: Box<dyn std::io::Read> = match &io.stdin_from {
                Some(path) => Box::new(std::io::BufReader::new(
                    std::fs::File::open(path)
                        .map_err(|e| anyhow::anyhow!("cannot open {}: {e}", path.display()))?,
                )),
                None => Box::new(std::io::empty()),
            };
            let mut captured = Vec::new();
            let mut file = match &io.stdout_to {
                Some(path) => Some(
                    std::fs::File::create(path)
                        .map_err(|e| anyhow::anyhow!("cannot create {}: {e}", path.display()))?,
                ),
                None => None,
            };
            let mut counted = CountingWriter::default();
            let json = {
                let out: &mut dyn std::io::Write = match file.as_mut() {
                    Some(f) => {
                        counted.inner = Some(f);
                        &mut counted
                    }
                    None => &mut captured,
                };
                crate::helper::run(&ctx, verb, &short, input, &mut reader, out)
                    .map_err(|e| anyhow::anyhow!("helper {verb} failed: {e}"))?
            };
            let streamed = counted.count;
            Ok(if verb == Verb::Backup {
                HelperOutput {
                    json,
                    stdout: captured,
                    streamed,
                }
            } else {
                HelperOutput {
                    stdout: serde_json::to_vec(&json).unwrap_or_default(),
                    json,
                    streamed,
                }
            })
        })
        .await
        .map_err(|e| anyhow::anyhow!("helper {verb} panicked: {e}"))?
    }
}

/// A writer that counts the bytes it passes on.
#[derive(Default)]
struct CountingWriter<'a> {
    inner: Option<&'a mut std::fs::File>,
    count: u64,
}

impl std::io::Write for CountingWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = match self.inner.as_mut() {
            Some(f) => f.write(buf)?,
            None => buf.len(),
        };
        self.count += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self.inner.as_mut() {
            Some(f) => f.flush(),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sudo_argv_is_exact() {
        assert_eq!(
            SudoHelper::new().argv(Verb::WriteEnv, "abcd1234"),
            [
                "sudo",
                "-n",
                "/opt/setu/setu",
                "helper",
                "write-env",
                "--site",
                "abcd1234"
            ]
        );
    }

    #[tokio::test]
    async fn sudo_helper_refuses_bad_short_before_spawning() {
        let err = SudoHelper::new()
            .call(
                Verb::Unit,
                "../etc",
                serde_json::json!({"op": "show"}),
                HelperIo::none(),
            )
            .await
            .expect_err("bad short");
        assert!(err.to_string().contains("invalid site id"), "{err}");
    }

    #[test]
    fn stdin_is_one_json_line_then_bytes() {
        let buf =
            frame_stdin(&serde_json::json!({"dirs": ["media"]}), Some(b"TAR")).expect("frame");
        assert_eq!(buf, b"{\"dirs\":[\"media\"]}\nTAR");
    }
}
