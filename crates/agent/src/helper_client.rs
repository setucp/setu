//! The agent's side of the sudo boundary: [`Helper`] runs one helper
//! verb and returns its JSON result (or, for `backup`, the archive).
//!
//! [`SudoHelper`] runs exactly `sudo -n /opt/setu/setu helper <verb>
//! --site <short>` (`-n`: never prompt, so a missing sudoers rule fails
//! fast instead of hanging the agent). [`FakeHelper`] records calls and
//! returns scripted outputs; [`LocalHelper`] runs the real helper code
//! in-process against a fake [`Ctx`] (tests).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use crate::helper::{Ctx, Verb};

/// The installed binary sudoers pins (root:root 0755, spec §4).
pub const HELPER_PATH: &str = "/opt/setu/setu";

/// One helper result.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HelperOutput {
    /// The JSON result (`Null` for `backup`).
    pub json: serde_json::Value,
    /// Raw stdout: the archive for `backup`, else the JSON text.
    pub stdout: Vec<u8>,
}

/// Run one privileged verb for one site.
#[async_trait::async_trait]
pub trait Helper: Send + Sync {
    async fn call(
        &self,
        verb: Verb,
        short: &str,
        input: serde_json::Value,
        stdin: Option<Vec<u8>>,
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

/// Parse helper stdout for `verb`.
pub fn parse_stdout(verb: Verb, stdout: Vec<u8>) -> anyhow::Result<HelperOutput> {
    if verb == Verb::Backup {
        return Ok(HelperOutput {
            json: serde_json::Value::Null,
            stdout,
        });
    }
    let json = if stdout.iter().all(u8::is_ascii_whitespace) {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&stdout)
            .map_err(|e| anyhow::anyhow!("helper {verb}: bad output: {e}"))?
    };
    Ok(HelperOutput { json, stdout })
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
        stdin: Option<Vec<u8>>,
    ) -> anyhow::Result<HelperOutput> {
        use tokio::io::AsyncWriteExt;
        if !setu_common::is_short_id(short) {
            anyhow::bail!("helper {verb}: invalid site id");
        }
        let argv = self.argv(verb, short);
        let bytes = frame_stdin(&input, stdin.as_deref())?;
        let mut child = tokio::process::Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| anyhow::anyhow!("cannot run helper {verb}: {e}"))?;
        let mut pipe = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("helper {verb}: no stdin"))?;
        // Write concurrently with reading stdout: a large restore archive
        // must not deadlock against a full stdout pipe.
        let writer = tokio::spawn(async move {
            let r = pipe.write_all(&bytes).await;
            drop(pipe);
            r
        });
        let out = child
            .wait_with_output()
            .await
            .map_err(|e| anyhow::anyhow!("helper {verb} failed: {e}"))?;
        let _ = writer.await;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            anyhow::bail!(
                "helper {verb} failed: {}",
                err.lines().next().unwrap_or("non-zero exit")
            );
        }
        parse_stdout(verb, out.stdout)
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
            .push_back(Ok(HelperOutput { json, stdout }));
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

#[async_trait::async_trait]
impl Helper for FakeHelper {
    async fn call(
        &self,
        verb: Verb,
        short: &str,
        input: serde_json::Value,
        stdin: Option<Vec<u8>>,
    ) -> anyhow::Result<HelperOutput> {
        let mut inner = self.lock();
        inner.calls.push(HelperCall {
            verb,
            short: short.to_string(),
            input: input.clone(),
            stdin,
        });
        match inner.scripted.get_mut(&verb).and_then(VecDeque::pop_front) {
            Some(Ok(out)) => Ok(out),
            Some(Err(msg)) => Err(anyhow::anyhow!("helper {verb} failed: {msg}")),
            None => {
                let json = default_output(verb, &input);
                Ok(HelperOutput {
                    stdout: if verb == Verb::Backup {
                        Vec::new()
                    } else {
                        serde_json::to_vec(&json).unwrap_or_default()
                    },
                    json,
                })
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
        stdin: Option<Vec<u8>>,
    ) -> anyhow::Result<HelperOutput> {
        let ctx = self.ctx.clone();
        let short = short.to_string();
        tokio::task::spawn_blocking(move || {
            let mut reader = std::io::Cursor::new(stdin.unwrap_or_default());
            let mut out = Vec::new();
            let json = crate::helper::run(&ctx, verb, &short, input, &mut reader, &mut out)
                .map_err(|e| anyhow::anyhow!("helper {verb} failed: {e}"))?;
            Ok(if verb == Verb::Backup {
                HelperOutput { json, stdout: out }
            } else {
                HelperOutput {
                    stdout: serde_json::to_vec(&json).unwrap_or_default(),
                    json,
                }
            })
        })
        .await
        .map_err(|e| anyhow::anyhow!("helper {verb} panicked: {e}"))?
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
                None,
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
