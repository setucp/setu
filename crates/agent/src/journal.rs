//! Site logs: `journalctl -u setu-site@<short> -o json` run as the agent
//! user (member of `systemd-journal`), never through the helper.

use crate::runtime::LogStream;

/// Largest cursor accepted from the control plane.
const MAX_CURSOR: usize = 512;

/// journalctl argv (after the program name).
pub fn journal_args(short: &str, cursor: Option<&str>, lines: u32) -> anyhow::Result<Vec<String>> {
    crate::paths::check_short(short)?;
    let mut args = vec![
        "-u".to_string(),
        crate::helper::unit_name(short),
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
            Some(serde_json::Value::String(s)) => lines.push(s.clone()),
            // Non-UTF-8 messages arrive as a byte array.
            Some(serde_json::Value::Array(bytes)) => {
                let raw: Vec<u8> = bytes
                    .iter()
                    .filter_map(|b| b.as_u64().and_then(|b| u8::try_from(b).ok()))
                    .collect();
                lines.push(String::from_utf8_lossy(&raw).into_owned());
            }
            _ => {}
        }
    }
    LogStream { lines, next }
}

/// Read up to `lines` entries after `cursor`.
pub async fn read(short: &str, cursor: Option<&str>, lines: u32) -> anyhow::Result<LogStream> {
    let args = journal_args(short, cursor, lines)?;
    let out = tokio::process::Command::new("journalctl")
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
}
