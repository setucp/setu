//! Site logs: `journalctl -u setu-site@<short> -o json` run as the agent
//! user (member of `systemd-journal`), never through the helper.

use crate::runtime::LogStream;

/// Largest cursor accepted from the control plane.
const MAX_CURSOR: usize = 512;

/// journalctl argv (after the program name).
pub fn journal_args(short: &str, cursor: Option<&str>, lines: u32) -> anyhow::Result<Vec<String>> {
    crate::paths::check_short(short)?;
    unit_args(&crate::helper::unit_name(short), cursor, lines)
}

/// journalctl argv for a host service from the fixed [`SystemUnit`] set.
pub fn system_journal_args(
    unit: setu_proto::SystemUnit,
    cursor: Option<&str>,
    lines: u32,
) -> anyhow::Result<Vec<String>> {
    unit_args(unit.unit_name(), cursor, lines)
}

fn unit_args(unit: &str, cursor: Option<&str>, lines: u32) -> anyhow::Result<Vec<String>> {
    let mut args = vec![
        "-u".to_string(),
        unit.to_string(),
        "-o".to_string(),
        "json".to_string(),
        "--no-pager".to_string(),
        "-n".to_string(),
        lines.clamp(1, 5000).to_string(),
    ];
    if let Some(c) = cursor {
        if c.is_empty() || c.len() > MAX_CURSOR || !setu_common::validate::no_control(c) {
            anyhow::bail!("invalid journal cursor");
        }
        // One argv element: the cursor can never become a flag.
        args.push(format!("--after-cursor={c}"));
    }
    Ok(args)
}

/// `MESSAGE` of each JSON line, plus the last `__CURSOR`.
pub fn parse_journal_json(text: &str) -> LogStream {
    let mut lines = Vec::new();
    let mut next = None;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if let Some(c) = v.get("__CURSOR").and_then(|c| c.as_str()) {
            next = Some(c.to_string());
        }
        match v.get("MESSAGE") {
            Some(serde_json::Value::String(s)) => lines.push(strip_ansi(s)),
            // Non-UTF-8 messages arrive as a byte array.
            Some(serde_json::Value::Array(bytes)) => {
                let raw: Vec<u8> = bytes
                    .iter()
                    .filter_map(|b| b.as_u64().and_then(|b| u8::try_from(b).ok()))
                    .collect();
                lines.push(strip_ansi(&String::from_utf8_lossy(&raw)));
            }
            _ => {}
        }
    }
    LogStream { lines, next }
}

/// Drops terminal escape sequences (colours, cursor moves, titles) from a
/// log line. Apps colour their logs whether or not a terminal is reading,
/// and the panel shows plain text, so `ESC[32mINFO` would arrive as
/// garbage. Handles CSI (`ESC [ … final`), OSC (`ESC ] … BEL` or
/// `ESC \\`) and two-byte `ESC x` escapes; any other text is kept.
pub fn strip_ansi(line: &str) -> String {
    if !line.contains('\u{1b}') {
        return line.to_owned();
    }
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.next() {
            // CSI: parameters and intermediates up to a final byte in @..~.
            Some('[') => {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            // OSC: up to BEL or the string terminator ESC \.
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Read up to `lines` entries after `cursor`.
/// `program` is `journalctl` in production (a fake script in tests).
pub async fn read(
    program: &std::path::Path,
    short: &str,
    cursor: Option<&str>,
    lines: u32,
) -> anyhow::Result<LogStream> {
    run(program, journal_args(short, cursor, lines)?, cursor).await
}

/// Like [`read`], for a host service.
pub async fn read_system(
    program: &std::path::Path,
    unit: setu_proto::SystemUnit,
    cursor: Option<&str>,
    lines: u32,
) -> anyhow::Result<LogStream> {
    run(program, system_journal_args(unit, cursor, lines)?, cursor).await
}

async fn run(
    program: &std::path::Path,
    args: Vec<String>,
    cursor: Option<&str>,
) -> anyhow::Result<LogStream> {
    let out = tokio::process::Command::new(program)
        .args(&args)
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("cannot run journalctl: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!(
            "journalctl failed: {}",
            err.lines().next().unwrap_or("non-zero exit")
        );
    }
    let mut stream = parse_journal_json(&String::from_utf8_lossy(&out.stdout));
    // No new entries: keep the caller's position.
    if stream.next.is_none() {
        stream.next = cursor.map(str::to_string);
    }
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_args_name_only_the_fixed_units() {
        use setu_proto::SystemUnit;
        let a = system_journal_args(SystemUnit::Caddy, Some("s=1;i=2"), 50).expect("args");
        assert_eq!(a[..2], ["-u", "caddy.service"]);
        assert_eq!(a.last().expect("last"), "--after-cursor=s=1;i=2");
        let a = system_journal_args(SystemUnit::Agent, None, 99_999).expect("args");
        assert_eq!(a[1], "setu-agent.service");
        assert!(a.contains(&"5000".to_string()), "lines are clamped");
        system_journal_args(SystemUnit::Caddy, Some("a\nb"), 1).expect_err("newline");
    }

    #[test]
    fn args_carry_unit_and_cursor_as_one_element() {
        let a = journal_args("abcd1234", Some("s=1;i=2"), 100).expect("args");
        assert_eq!(a[..2], ["-u", "setu-site@abcd1234.service"]);
        assert!(a.contains(&"json".to_string()));
        assert_eq!(a.last().expect("last"), "--after-cursor=s=1;i=2");
        journal_args("abcd1234", Some("x\ny"), 1).expect_err("newline");
        journal_args("../x", None, 1).expect_err("short");
    }

    #[test]
    fn parses_messages_and_last_cursor() {
        let text = concat!(
            "{\"__CURSOR\":\"c1\",\"MESSAGE\":\"hello\"}\n",
            "{\"__CURSOR\":\"c2\",\"MESSAGE\":[104,105]}\n",
            "not json\n"
        );
        let s = parse_journal_json(text);
        assert_eq!(s.lines, ["hello", "hi"]);
        assert_eq!(s.next.as_deref(), Some("c2"));
    }

    #[test]
    fn colour_codes_are_stripped_from_messages() {
        // What a tracing subscriber writes when it thinks a terminal is
        // reading: dim timestamp, green level, dim target.
        let coloured = "\u{1b}[2m2026-10-22T06:37:20Z\u{1b}[0m \u{1b}[32m INFO\u{1b}[0m \u{1b}[2mvyasa::host\u{1b}[0m\u{1b}[2m:\u{1b}[0m docs sync finished";
        let text = format!(
            "{}\n{}\n",
            serde_json::json!({"__CURSOR": "c1", "MESSAGE": coloured}),
            // journald hands control characters over as a byte array.
            serde_json::json!({"__CURSOR": "c2", "MESSAGE": "\u{1b}[31mfail\u{1b}[0m".bytes().collect::<Vec<_>>()}),
        );
        let s = parse_journal_json(&text);
        assert_eq!(
            s.lines,
            [
                "2026-10-22T06:37:20Z  INFO vyasa::host: docs sync finished",
                "fail"
            ]
        );
    }

    #[test]
    fn strip_ansi_keeps_plain_text_and_drops_osc_titles() {
        assert_eq!(strip_ansi("plain [text] ok"), "plain [text] ok");
        assert_eq!(strip_ansi("\u{1b}]0;title\u{7}after"), "after");
        assert_eq!(
            strip_ansi("a\u{1b}]8;;http://x\u{1b}\\link\u{1b}]8;;\u{1b}\\b"),
            "alinkb"
        );
        assert_eq!(strip_ansi("bold \u{1b}[1;31mred\u{1b}[0m"), "bold red");
    }
}
