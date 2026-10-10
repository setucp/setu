//! `setu helper` verbs, unprivileged: a tempdir root, `Recording`
//! ownership, `FakeUsers`, `FakeSystemctl` (plan Task 10b).

use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use setu_agent::helper::{self, Ctx, FakeSystemctl, FakeUsers, HelperError, Recording, Verb};

const S: &str = "abcd1234";

struct T {
    dir: tempfile::TempDir,
    own: Recording,
    users: FakeUsers,
    units: FakeSystemctl,
    ctx: Ctx,
}

fn setup() -> T {
    let dir = tempfile::tempdir().expect("tempdir");
    let own = Recording::new();
    let users = FakeUsers::new();
    let units = FakeSystemctl::new();
    let ctx = Ctx {
        root: dir.path().to_path_buf(),
        users: Box::new(users.clone()),
        own: Box::new(own.clone()),
        units: Box::new(units.clone()),
    };
    T {
        dir,
        own,
        users,
        units,
        ctx,
    }
}

impl T {
    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn site(&self, rest: &str) -> PathBuf {
        let p = self.root().join("srv/setu/sites").join(S);
        if rest.is_empty() {
            p
        } else {
            p.join(rest)
        }
    }

    fn run(&self, verb: Verb, short: &str, input: Value) -> Result<Value, HelperError> {
        helper::run(
            &self.ctx,
            verb,
            short,
            input,
            &mut std::io::empty(),
            &mut Vec::new(),
        )
    }

    fn run_io(
        &self,
        verb: Verb,
        input: Value,
        stdin: &[u8],
    ) -> Result<(Value, Vec<u8>), HelperError> {
        let mut out = Vec::new();
        let v = helper::run(
            &self.ctx,
            verb,
            S,
            input,
            &mut std::io::Cursor::new(stdin),
            &mut out,
        )?;
        Ok((v, out))
    }

    /// The files this test writes are owned by us: make us the site
    /// user, as root's chown would have (backup skips other owners).
    fn files_belong_to_site_user(&self) {
        let (uid, gid) = (
            nix::unistd::getuid().as_raw(),
            nix::unistd::getgid().as_raw(),
        );
        self.users.set(S, uid, gid);
    }

    fn create(&self, dirs: &[&str]) {
        self.run(Verb::SiteCreate, S, json!({ "dirs": dirs }))
            .expect("site-create");
    }

    /// Put `bytes` where the agent would: downloads/<sha>.tar.gz.
    fn put_download(&self, bytes: &[u8]) -> String {
        let sha = sha256(bytes);
        let dl = self.root().join("var/lib/setu/downloads");
        std::fs::create_dir_all(&dl).expect("mkdir downloads");
        std::fs::write(dl.join(format!("{sha}.tar.gz")), bytes).expect("write download");
        sha
    }
}

fn sha256(b: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(b))
}

/// Every path under `root` (relative, sorted), never following links.
fn snapshot(root: &Path) -> Vec<PathBuf> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            out.push(p.strip_prefix(base).expect("prefix").to_path_buf());
            if e.file_type().expect("type").is_dir() {
                walk(base, &p, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .expect("read_dir")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn mode(p: &Path) -> u32 {
    std::fs::symlink_metadata(p)
        .expect("stat")
        .permissions()
        .mode()
        & 0o7777
}

/// One tar entry; names are written raw so hostile ones survive.
struct E<'a> {
    name: &'a [u8],
    kind: tar::EntryType,
    mode: u32,
    data: &'a [u8],
    link: Option<&'a str>,
}

fn file<'a>(name: &'a str, mode: u32, data: &'a [u8]) -> E<'a> {
    E {
        name: name.as_bytes(),
        kind: tar::EntryType::Regular,
        mode,
        data,
        link: None,
    }
}

fn dir_e(name: &str) -> E<'_> {
    E {
        name: name.as_bytes(),
        kind: tar::EntryType::Directory,
        mode: 0o755,
        data: b"",
        link: None,
    }
}

fn tar_of(entries: &[E<'_>]) -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    for e in entries {
        let mut h = tar::Header::new_gnu();
        h.as_old_mut().name[..e.name.len()].copy_from_slice(e.name);
        h.set_entry_type(e.kind);
        h.set_mode(e.mode);
        h.set_size(e.data.len() as u64);
        if let Some(l) = e.link {
            h.set_link_name(l).expect("link name");
        }
        h.set_cksum();
        b.append(&h, e.data).expect("append");
    }
    b.into_inner().expect("finish")
}

fn gz(b: &[u8]) -> Vec<u8> {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(b).expect("gz");
    enc.finish().expect("gz finish")
}

fn zst(b: &[u8]) -> Vec<u8> {
    zstd::encode_all(b, 3).expect("zstd")
}

fn unit_input(args: &[&str]) -> Value {
    json!({
        "short_id": S,
        "version": "0.1.0",
        "exec": "dist/app",
        "args": args,
        "workdir": "exec_dir",
        "memory_mb": 512,
        "cpu_percent": 50
    })
}

fn dropin_path(t: &T) -> PathBuf {
    t.root()
        .join("etc/systemd/system")
        .join(format!("setu-site@{S}.service.d"))
        .join("10-setu.conf")
}

#[test]
fn helper_rejects_bad_site_id() {
    let t = setup();
    let all = [
        (Verb::SiteCreate, json!({"dirs": []})),
        (Verb::WriteUnit, unit_input(&[])),
        (Verb::WriteEnv, json!([])),
        (
            Verb::Install,
            json!({"version": "0.1.0", "sha256": "a".repeat(64)}),
        ),
        (Verb::SwitchVersion, json!({"version": "0.1.0"})),
        (Verb::Unit, json!({"op": "start"})),
        (Verb::Backup, json!({"dirs": []})),
        (Verb::Restore, json!({"dirs": []})),
        (Verb::SiteRemove, json!({})),
    ];
    for bad in [
        "",
        "ABCD1234",
        "abcd123",
        "abcd12345",
        "../etc/",
        "abcd/234",
        "abcd 234",
        "abcd123\n",
    ] {
        for (verb, input) in &all {
            let err = t.run(*verb, bad, input.clone()).expect_err("bad site id");
            assert!(
                matches!(err, HelperError::Invalid(_)),
                "{verb} {bad:?}: {err:?}"
            );
        }
    }
    assert!(snapshot(t.root()).is_empty(), "nothing may be written");
    assert!(t.users.calls().is_empty());
    assert!(t.units.calls().is_empty());
}

#[test]
fn helper_rejects_bad_version() {
    let t = setup();
    t.create(&[]);
    let before = snapshot(t.root());
    for bad in ["../x", "..", "a/b", "", "1 0", "v1\n", &"9".repeat(65)] {
        let sha = "a".repeat(64);
        for (verb, input) in [
            (Verb::Install, json!({"version": bad, "sha256": sha})),
            (Verb::SwitchVersion, json!({"version": bad})),
            (Verb::WriteUnit, {
                let mut u = unit_input(&[]);
                u["version"] = json!(bad);
                u
            }),
        ] {
            let err = t.run(verb, S, input).expect_err("bad version");
            assert!(
                matches!(err, HelperError::Invalid(_)),
                "{verb} {bad:?}: {err:?}"
            );
        }
    }
    // A bad sha256 is refused the same way (it names a file).
    for bad in ["../../etc/shadow", &"A".repeat(64), "abc"] {
        let err = t
            .run(Verb::Install, S, json!({"version": "0.1.0", "sha256": bad}))
            .expect_err("bad sha");
        assert!(matches!(err, HelperError::Invalid(_)), "{err:?}");
    }
    assert_eq!(snapshot(t.root()), before, "nothing written");
}

#[test]
fn helper_rejects_bad_dir_name() {
    let t = setup();
    for bad in [
        json!(["../x"]),
        json!(["Media"]),
        json!(["a/b"]),
        json!([""]),
        json!(["media", "media"]),
        json!(["-x"]),
    ] {
        for verb in [Verb::SiteCreate, Verb::Backup, Verb::Restore] {
            let err = t.run(verb, S, json!({ "dirs": bad })).expect_err("bad dir");
            assert!(
                matches!(err, HelperError::Invalid(_)),
                "{verb} {bad}: {err:?}"
            );
        }
    }
    // Unknown fields are refused too (typed input only).
    let err = t
        .run(Verb::SiteCreate, S, json!({"dirs": [], "path": "/etc"}))
        .expect_err("unknown field");
    assert!(matches!(err, HelperError::Invalid(_)), "{err:?}");
    assert!(snapshot(t.root()).is_empty(), "nothing written");
    assert!(t.users.calls().is_empty(), "no user created");
}

/// Re-exec target for `helper_refuses_root_flag_under_sudo`: runs the
/// real CLI entry (`setu helper …`) when `SETU_HELPER_CHILD` is set.
#[test]
fn zz_helper_child_entry() {
    use clap::Parser;
    #[derive(clap::Parser)]
    struct P {
        #[command(flatten)]
        args: setu_agent::helper::cli::HelperArgs,
    }
    let Ok(argv) = std::env::var("SETU_HELPER_CHILD") else {
        return;
    };
    let argv: Vec<String> = serde_json::from_str(&argv).expect("argv json");
    let p = P::try_parse_from(std::iter::once("helper".to_string()).chain(argv)).expect("parse");
    std::process::exit(setu_agent::helper::cli::main(p.args));
}

fn child(root: &Path, verb: &str, input: &str, sudo_uid: Option<&str>) -> std::process::Output {
    use std::process::{Command, Stdio};
    let argv = json!([verb, "--site", S, "--root", root]);
    let mut cmd = Command::new(std::env::current_exe().expect("exe"));
    cmd.args([
        "--exact",
        "zz_helper_child_entry",
        "--nocapture",
        "--test-threads=1",
    ])
    .env("SETU_HELPER_CHILD", argv.to_string())
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    match sudo_uid {
        Some(uid) => cmd.env("SUDO_UID", uid),
        None => cmd.env_remove("SUDO_UID"),
    };
    let mut c = cmd.spawn().expect("spawn child");
    c.stdin
        .take()
        .expect("stdin")
        .write_all(format!("{input}\n").as_bytes())
        .expect("write stdin");
    c.wait_with_output().expect("wait")
}

#[test]
fn helper_refuses_root_flag_under_sudo() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Under sudo (SUDO_UID set) `--root` is refused before anything.
    let out = child(
        dir.path(),
        "site-create",
        r#"{"dirs":["media"]}"#,
        Some("1000"),
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("--root is not allowed under sudo"));
    assert!(
        snapshot(dir.path()).is_empty(),
        "nothing written under sudo"
    );

    // The same call without SUDO_UID works (proves the refusal is the gate).
    let out = child(dir.path(), "site-create", r#"{"dirs":["media"]}"#, None);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(dir
        .path()
        .join("srv/setu/sites")
        .join(S)
        .join("data/media")
        .is_dir());

    // And a write under sudo is refused with the tree in place.
    let out = child(
        dir.path(),
        "write-env",
        r#"[{"name":"A","value":"b"}]"#,
        Some("0"),
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(!dir
        .path()
        .join("srv/setu/sites")
        .join(S)
        .join("env")
        .exists());
}

#[test]
fn site_create_is_idempotent_and_records_ownership() {
    let t = setup();
    let out = t
        .run(Verb::SiteCreate, S, json!({"dirs": ["media", "uploads"]}))
        .expect("first");
    let (uid, gid) = t.users.get(S).expect("site user");
    assert!((30000..=39999).contains(&uid));
    assert_eq!(out["uid"], uid);
    assert_eq!(out["user"], format!("setu-site-{S}"));

    for (rel, owner, m) in [
        ("", (0, 0), 0o755),
        ("bin", (0, 0), 0o755),
        ("data", (uid, gid), 0o750),
        ("run", (uid, gid), 0o750),
        ("data/media", (uid, gid), 0o750),
        ("data/uploads", (uid, gid), 0o750),
    ] {
        let p = t.site(rel);
        assert!(p.is_dir(), "{rel} exists");
        assert_eq!(t.own.owner_of(&p), Some(owner), "{rel} owner");
        assert_eq!(t.own.mode_of(&p), Some(m), "{rel} recorded mode");
        assert_eq!(mode(&p), m, "{rel} mode on disk");
    }
    assert!(!t.site("env").exists(), "env is write-env's job");
    assert!(!t.own.touched(&t.site("env")));

    // Idempotent: same tree, same user, no error.
    let before = snapshot(t.root());
    let again = t
        .run(Verb::SiteCreate, S, json!({"dirs": ["media", "uploads"]}))
        .expect("again");
    assert_eq!(again["uid"], uid);
    assert_eq!(snapshot(t.root()), before);
    assert_eq!(t.users.get(S), Some((uid, gid)));
    // No special bit anywhere.
    for p in snapshot(t.root()) {
        assert_eq!(mode(&t.root().join(&p)) & 0o7000, 0, "{}", p.display());
    }
}

#[test]
fn write_env_is_atomic_and_records_0600_root() {
    let t = setup();
    t.create(&[]);
    let env = t.site("env");
    let out = t
        .run(Verb::WriteEnv, S, json!([{"name": "APP_PORT", "value": "20001"}, {"name": "SECRET", "value": "s3 \"q\" $x"}]))
        .expect("write-env");
    assert_eq!(out["changed"], true);
    assert_eq!(
        std::fs::read_to_string(&env).expect("read"),
        "APP_PORT=\"20001\"\nSECRET=\"s3 \\\"q\\\" \\$x\"\n"
    );
    assert_eq!(mode(&env), 0o600);
    assert_eq!(t.own.owner_of(&env), Some((0, 0)));
    assert_eq!(t.own.mode_of(&env), Some(0o600));
    let ino = std::fs::metadata(&env).expect("stat").ino();

    // A rewrite replaces the file by rename (new inode, no temp left).
    t.run(
        Verb::WriteEnv,
        S,
        json!([{"name": "APP_PORT", "value": "20002"}]),
    )
    .expect("rewrite");
    assert_ne!(
        std::fs::metadata(&env).expect("stat").ino(),
        ino,
        "rename, not in-place"
    );
    assert_eq!(
        std::fs::read_to_string(&env).expect("read"),
        "APP_PORT=\"20002\"\n"
    );
    assert_eq!(
        names(&t.site("")),
        ["bin", "data", "env", "run"],
        "no temp files"
    );
    let same = t
        .run(
            Verb::WriteEnv,
            S,
            json!([{"name": "APP_PORT", "value": "20002"}]),
        )
        .expect("same");
    assert_eq!(same["changed"], false);

    // A newline in a value is refused; the error never echoes the value.
    let err = t
        .run(
            Verb::WriteEnv,
            S,
            json!([{"name": "A", "value": "x\nINJECTED=1"}]),
        )
        .expect_err("newline");
    assert!(matches!(err, HelperError::Invalid(_)));
    assert!(!err.to_string().contains("INJECTED"), "{err}");
    let err = t
        .run(
            Verb::WriteEnv,
            S,
            json!([{"name": "bad-name", "value": "x"}]),
        )
        .expect_err("name");
    assert!(matches!(err, HelperError::Invalid(_)));
    assert_eq!(
        std::fs::read_to_string(&env).expect("read"),
        "APP_PORT=\"20002\"\n"
    );
}

#[test]
fn helper_refuses_symlinked_env() {
    let t = setup();
    t.create(&[]);
    let target = t.root().join("victim");
    std::fs::write(&target, b"original").expect("write victim");
    std::os::unix::fs::symlink(&target, t.site("env")).expect("symlink env");
    let err = t
        .run(Verb::WriteEnv, S, json!([{"name": "A", "value": "b"}]))
        .expect_err("symlinked env");
    assert!(matches!(err, HelperError::Refused(_)), "{err:?}");
    assert_eq!(std::fs::read(&target).expect("read"), b"original");
    assert!(std::fs::symlink_metadata(t.site("env"))
        .expect("lstat")
        .file_type()
        .is_symlink());
    assert_eq!(
        names(&t.site("")),
        ["bin", "data", "env", "run"],
        "no temp files"
    );
}

#[test]
fn helper_refuses_symlinked_parent() {
    // The site dir itself is a symlink to an attacker-chosen tree.
    let t = setup();
    let elsewhere = t.root().join("elsewhere");
    std::fs::create_dir_all(elsewhere.join("bin")).expect("mkdir");
    std::fs::create_dir_all(t.root().join("srv/setu/sites")).expect("mkdir");
    std::os::unix::fs::symlink(&elsewhere, t.site("")).expect("symlink site");
    for (verb, input) in [
        (Verb::WriteEnv, json!([{"name": "A", "value": "b"}])),
        (Verb::SiteCreate, json!({"dirs": ["media"]})),
        (Verb::SwitchVersion, json!({"version": "0.1.0"})),
        (Verb::Backup, json!({"dirs": ["media"]})),
    ] {
        let err = t.run(verb, S, input).expect_err("symlinked parent");
        assert!(matches!(err, HelperError::Refused(_)), "{verb}: {err:?}");
    }
    assert_eq!(
        names(&elsewhere),
        ["bin"],
        "nothing written through the link"
    );

    // A symlinked component higher up (`srv/setu`) is refused as well.
    let t = setup();
    let other = t.root().join("other/sites").join(S);
    std::fs::create_dir_all(&other).expect("mkdir");
    std::fs::create_dir_all(t.root().join("srv")).expect("mkdir");
    std::os::unix::fs::symlink(t.root().join("other"), t.root().join("srv/setu")).expect("symlink");
    let err = t
        .run(Verb::WriteEnv, S, json!([{"name": "A", "value": "b"}]))
        .expect_err("refused");
    assert!(matches!(err, HelperError::Refused(_)), "{err:?}");
    assert!(!other.join("env").exists());

    // A site user's symlink inside data/ is never followed by site-create.
    let t = setup();
    t.create(&[]);
    let outside = t.root().join("outside");
    std::fs::create_dir_all(&outside).expect("mkdir");
    std::os::unix::fs::symlink(&outside, t.site("data/media")).expect("symlink media");
    let err = t
        .run(Verb::SiteCreate, S, json!({"dirs": ["media"]}))
        .expect_err("refused");
    assert!(matches!(err, HelperError::Refused(_)), "{err:?}");
    assert!(
        !t.own.touched(&t.site("data/media")),
        "no chown through the link"
    );
}

#[test]
fn write_unit_renders_golden_dropin() {
    let t = setup();
    let out = t
        .run(
            Verb::WriteUnit,
            S,
            unit_input(&["serve", "--bind", "127.0.0.1:20001", "100%", "$HOME", "a b"]),
        )
        .expect("write-unit");
    assert_eq!(out["changed"], true);
    let golden = include_str!("golden/dropin-abcd1234.conf");
    let path = dropin_path(&t);
    assert_eq!(std::fs::read_to_string(&path).expect("read dropin"), golden);
    assert_eq!(mode(&path), 0o644);
    assert_eq!(t.own.owner_of(&path), Some((0, 0)));
    assert_eq!(t.units.reloads(), 1, "daemon-reload after a change");

    // Workdir variants and no-args form.
    let mut u = unit_input(&[]);
    u["exec"] = json!("app");
    u["workdir"] = json!("data");
    t.run(Verb::WriteUnit, S, u).expect("data workdir");
    let text = std::fs::read_to_string(&path).expect("read");
    assert!(
        text.contains(&format!("ExecStart=/srv/setu/sites/{S}/bin/current/app\n")),
        "{text}"
    );
    assert!(
        text.contains(&format!("WorkingDirectory=/srv/setu/sites/{S}/data\n")),
        "{text}"
    );
}

#[test]
fn write_unit_rejects_newline_percent_and_dollar() {
    let t = setup();
    // Args: control characters refused; `%`/`$` are escaped, never raw.
    for bad in ["a\nExecStartPre=/bin/sh", "a\rb", "tab\there", "nul\0"] {
        let err = t
            .run(Verb::WriteUnit, S, unit_input(&[bad]))
            .expect_err("control char");
        assert!(matches!(err, HelperError::Invalid(_)), "{bad:?}: {err:?}");
    }
    // exec: only `[A-Za-z0-9._-]` segments, no `..`.
    for bad in ["app%n", "app$X", "../app", "a/../b", "/abs", "app\n", "a b"] {
        let mut u = unit_input(&[]);
        u["exec"] = json!(bad);
        let err = t.run(Verb::WriteUnit, S, u).expect_err("bad exec");
        assert!(matches!(err, HelperError::Invalid(_)), "{bad:?}: {err:?}");
    }
    // short_id must match --site; limits must be positive.
    let mut u = unit_input(&[]);
    u["short_id"] = json!("zzzz9999");
    assert!(matches!(
        t.run(Verb::WriteUnit, S, u),
        Err(HelperError::Invalid(_))
    ));
    let mut u = unit_input(&[]);
    u["memory_mb"] = json!(0);
    assert!(matches!(
        t.run(Verb::WriteUnit, S, u),
        Err(HelperError::Invalid(_))
    ));
    assert!(snapshot(t.root()).is_empty(), "nothing written");
    assert_eq!(t.units.reloads(), 0);

    t.run(Verb::WriteUnit, S, unit_input(&["50%", "$PATH", "${X}"]))
        .expect("escaped");
    let text = std::fs::read_to_string(dropin_path(&t)).expect("read");
    assert!(text.contains(" \"50%%\" \"$$PATH\" \"$${X}\"\n"), "{text}");
    assert_eq!(text.lines().count(), 6, "no injected lines: {text}");
}

#[test]
fn write_unit_returns_changed_false_when_identical() {
    let t = setup();
    let first = t
        .run(Verb::WriteUnit, S, unit_input(&["serve"]))
        .expect("first");
    assert_eq!(first["changed"], true);
    let second = t
        .run(Verb::WriteUnit, S, unit_input(&["serve"]))
        .expect("second");
    assert_eq!(second["changed"], false);
    assert_eq!(t.units.reloads(), 1, "no reload when unchanged");
    let mut u = unit_input(&["serve"]);
    u["memory_mb"] = json!(1024);
    assert_eq!(
        t.run(Verb::WriteUnit, S, u).expect("third")["changed"],
        true
    );
    assert_eq!(t.units.reloads(), 2);
    let dir = dropin_path(&t).parent().expect("parent").to_path_buf();
    assert_eq!(names(&dir), ["10-setu.conf"], "no temp files");
}

#[test]
fn install_rejects_traversal_symlink_hardlink_and_special_bits() {
    let t = setup();
    t.create(&[]);
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("dotdot", tar_of(&[file("../escaped", 0o644, b"x")])),
        (
            "nested dotdot",
            tar_of(&[file("a/../../escaped", 0o644, b"x")]),
        ),
        ("absolute", tar_of(&[file("/tmp/escaped", 0o644, b"x")])),
        (
            "symlink",
            tar_of(&[
                file("app", 0o755, b"bin"),
                E {
                    name: b"link",
                    kind: tar::EntryType::Symlink,
                    mode: 0o777,
                    data: b"",
                    link: Some("/etc/shadow"),
                },
            ]),
        ),
        (
            "hardlink",
            tar_of(&[
                file("app", 0o755, b"bin"),
                E {
                    name: b"hard",
                    kind: tar::EntryType::Link,
                    mode: 0o644,
                    data: b"",
                    link: Some("app"),
                },
            ]),
        ),
        (
            "char device",
            tar_of(&[E {
                name: b"dev",
                kind: tar::EntryType::Char,
                mode: 0o644,
                data: b"",
                link: None,
            }]),
        ),
        (
            "block device",
            tar_of(&[E {
                name: b"blk",
                kind: tar::EntryType::Block,
                mode: 0o644,
                data: b"",
                link: None,
            }]),
        ),
        (
            "fifo",
            tar_of(&[E {
                name: b"fifo",
                kind: tar::EntryType::Fifo,
                mode: 0o644,
                data: b"",
                link: None,
            }]),
        ),
        ("setuid", tar_of(&[file("app", 0o4755, b"bin")])),
        ("setgid", tar_of(&[file("app", 0o2755, b"bin")])),
        (
            "sticky dir",
            tar_of(&[E {
                name: b"d/",
                kind: tar::EntryType::Directory,
                mode: 0o1777,
                data: b"",
                link: None,
            }]),
        ),
        (
            "duplicate",
            tar_of(&[file("app", 0o755, b"one"), file("app", 0o755, b"two")]),
        ),
    ];
    for (case, archive) in cases {
        let sha = t.put_download(&gz(&archive));
        let err = t
            .run(Verb::Install, S, json!({"version": "0.1.0", "sha256": sha}))
            .expect_err(case);
        assert!(matches!(err, HelperError::Refused(_)), "{case}: {err:?}");
        assert!(
            names(&t.site("bin")).is_empty(),
            "{case}: nothing left in bin/"
        );
        assert!(
            !t.root().join("escaped").exists() && !t.root().join("srv/setu/sites/escaped").exists()
        );
        assert!(!t.site("escaped").exists());
    }
}

#[test]
fn install_requires_regular_download_file() {
    let t = setup();
    t.create(&[]);
    let good = gz(&tar_of(&[file("app", 0o755, b"bin")]));
    let sha = sha256(&good);
    let dl = t.root().join("var/lib/setu/downloads");
    std::fs::create_dir_all(&dl).expect("mkdir");
    let real = t.root().join("real.tar.gz");
    std::fs::write(&real, &good).expect("write");
    let name = dl.join(format!("{sha}.tar.gz"));
    let input = json!({"version": "0.1.0", "sha256": sha});

    // Missing.
    t.run(Verb::Install, S, input.clone()).expect_err("missing");
    // A symlink to a perfectly good archive is still refused.
    std::os::unix::fs::symlink(&real, &name).expect("symlink");
    let err = t.run(Verb::Install, S, input.clone()).expect_err("symlink");
    assert!(matches!(err, HelperError::Refused(_)), "{err:?}");
    std::fs::remove_file(&name).expect("rm");
    // A fifo (would hang a naive open) and a directory.
    let st = std::process::Command::new("mkfifo")
        .arg(&name)
        .status()
        .expect("mkfifo");
    assert!(st.success());
    let err = t.run(Verb::Install, S, input.clone()).expect_err("fifo");
    assert!(matches!(err, HelperError::Refused(_)), "{err:?}");
    std::fs::remove_file(&name).expect("rm");
    std::fs::create_dir(&name).expect("mkdir");
    let err = t.run(Verb::Install, S, input.clone()).expect_err("dir");
    assert!(matches!(err, HelperError::Refused(_)), "{err:?}");
    std::fs::remove_dir(&name).expect("rmdir");
    // Wrong bytes under the right name.
    std::fs::write(&name, b"not the archive").expect("write");
    let err = t.run(Verb::Install, S, input.clone()).expect_err("hash");
    assert!(matches!(err, HelperError::Refused(_)), "{err:?}");
    assert!(names(&t.site("bin")).is_empty());
    // The real thing works.
    std::fs::write(&name, &good).expect("write");
    t.run(Verb::Install, S, input).expect("install");
    assert!(t.site("bin/0.1.0/app").is_file());
}

#[test]
fn install_extracts_and_switch_version_is_atomic() {
    let t = setup();
    t.create(&[]);
    let v1 = gz(&tar_of(&[
        dir_e("./"),
        file("./app", 0o750, b"v1"),
        dir_e("share/"),
        file("share/readme", 0o600, b"doc"),
        file("deep/nested/x", 0o644, b"x"),
    ]));
    let sha1 = t.put_download(&v1);
    let out = t
        .run(
            Verb::Install,
            S,
            json!({"version": "0.1.0", "sha256": sha1}),
        )
        .expect("install");
    assert_eq!(out["changed"], true);
    let vdir = t.site("bin/0.1.0");
    assert_eq!(std::fs::read(vdir.join("app")).expect("app"), b"v1");
    assert_eq!(mode(&vdir.join("app")), 0o755, "executables 0755");
    assert_eq!(mode(&vdir.join("share/readme")), 0o644, "files 0644");
    assert_eq!(mode(&vdir), 0o755);
    assert!(vdir.join("deep/nested/x").is_file());
    assert_eq!(t.own.owner_of(&vdir), Some((0, 0)));
    // Re-install is a no-op.
    let again = t
        .run(
            Verb::Install,
            S,
            json!({"version": "0.1.0", "sha256": sha1}),
        )
        .expect("again");
    assert_eq!(again["changed"], false);

    // Switching to a version that is not installed fails, nothing moves.
    let err = t
        .run(Verb::SwitchVersion, S, json!({"version": "0.2.0"}))
        .expect_err("missing");
    assert!(matches!(err, HelperError::Invalid(_)), "{err:?}");
    assert!(!t.site("bin/current").exists());

    let out = t
        .run(Verb::SwitchVersion, S, json!({"version": "0.1.0"}))
        .expect("switch");
    assert_eq!(out["changed"], true);
    let current = t.site("bin/current");
    assert_eq!(
        std::fs::read_link(&current).expect("link"),
        PathBuf::from("0.1.0")
    );
    assert_eq!(
        std::fs::read(current.join("app")).expect("via current"),
        b"v1"
    );
    let ino1 = std::fs::symlink_metadata(&current).expect("lstat").ino();
    assert_eq!(
        t.run(Verb::SwitchVersion, S, json!({"version": "0.1.0"}))
            .expect("same")["changed"],
        false
    );

    let sha2 = t.put_download(&gz(&tar_of(&[file("app", 0o755, b"v2")])));
    t.run(
        Verb::Install,
        S,
        json!({"version": "0.2.0", "sha256": sha2}),
    )
    .expect("install v2");
    // Installing beside does not move `current`.
    assert_eq!(
        std::fs::read_link(&current).expect("link"),
        PathBuf::from("0.1.0")
    );
    t.run(Verb::SwitchVersion, S, json!({"version": "0.2.0"}))
        .expect("switch v2");
    assert_eq!(
        std::fs::read_link(&current).expect("link"),
        PathBuf::from("0.2.0")
    );
    // A new link renamed over the old one: new inode, no temp left.
    assert_ne!(
        std::fs::symlink_metadata(&current).expect("lstat").ino(),
        ino1
    );
    assert_eq!(names(&t.site("bin")), ["0.1.0", "0.2.0", "current"]);
}

fn list_tar_zst(bytes: &[u8]) -> Vec<String> {
    let dec = zstd::stream::read::Decoder::new(bytes).expect("zstd");
    let mut ar = tar::Archive::new(dec);
    let mut v: Vec<String> = ar
        .entries()
        .expect("entries")
        .map(|e| {
            e.expect("entry")
                .path()
                .expect("path")
                .display()
                .to_string()
        })
        .collect();
    v.sort();
    v
}

#[test]
fn backup_streams_tar_and_restore_swaps_dirs() {
    let t = setup();
    t.create(&["media", "uploads"]);
    t.files_belong_to_site_user();
    std::fs::write(t.site("data/media/a.txt"), b"alpha").expect("write");
    std::fs::create_dir_all(t.site("data/media/sub")).expect("mkdir");
    std::fs::write(t.site("data/media/sub/b.txt"), b"beta").expect("write");
    std::fs::write(t.site("data/uploads/u.bin"), b"up").expect("write");
    std::fs::write(t.site("data/not-backed-up"), b"skip").expect("write");
    // A site user's symlink to a root-only file is never archived.
    let secret = t.root().join("shadow");
    std::fs::write(&secret, b"root only").expect("write");
    std::os::unix::fs::symlink(&secret, t.site("data/media/leak")).expect("symlink");

    let (json_out, archive) = t
        .run_io(
            Verb::Backup,
            json!({"dirs": ["media", "uploads", "never-created"]}),
            b"",
        )
        .expect("backup");
    assert!(json_out.is_null());
    assert_eq!(
        list_tar_zst(&archive),
        [
            "media",
            "media/a.txt",
            "media/sub",
            "media/sub/b.txt",
            "uploads",
            "uploads/u.bin"
        ]
    );

    // Change the live data, then restore the backup over it.
    std::fs::write(t.site("data/media/a.txt"), b"changed").expect("write");
    std::fs::write(t.site("data/media/new.txt"), b"new").expect("write");
    std::fs::remove_file(t.site("data/uploads/u.bin")).expect("rm");
    let (out, _) = t
        .run_io(
            Verb::Restore,
            json!({"dirs": ["media", "uploads"]}),
            &archive,
        )
        .expect("restore");
    assert_eq!(out["dirs"], json!(["media", "uploads"]));
    assert_eq!(
        std::fs::read(t.site("data/media/a.txt")).expect("a"),
        b"alpha"
    );
    assert_eq!(
        std::fs::read(t.site("data/media/sub/b.txt")).expect("b"),
        b"beta"
    );
    assert_eq!(
        std::fs::read(t.site("data/uploads/u.bin")).expect("u"),
        b"up"
    );
    assert!(
        !t.site("data/media/new.txt").exists(),
        "old dir replaced, not merged"
    );
    assert!(
        !t.site("data/media/leak").exists(),
        "old dir (with its link) removed"
    );
    assert_eq!(
        std::fs::read(t.site("data/not-backed-up")).expect("other"),
        b"skip"
    );
    assert_eq!(std::fs::read(&secret).expect("secret"), b"root only");
    // Staging was used and removed; the result belongs to the site user.
    assert_eq!(
        names(&t.site("")),
        ["bin", "data", "run"],
        "no staging left"
    );
    let (uid, gid) = t.users.get(S).expect("user");
    for rel in [
        "data/media",
        "data/media/a.txt",
        "data/media/sub/b.txt",
        "data/uploads/u.bin",
    ] {
        assert_eq!(t.own.owner_of(&t.site(rel)), Some((uid, gid)), "{rel}");
    }
    assert_eq!(mode(&t.site("data/media")), 0o750);
}

#[test]
fn restore_refuses_when_unit_active() {
    let t = setup();
    t.create(&["media"]);
    t.files_belong_to_site_user();
    std::fs::write(t.site("data/media/a.txt"), b"live").expect("write");
    let (_, archive) = t
        .run_io(Verb::Backup, json!({"dirs": ["media"]}), b"")
        .expect("backup");
    std::fs::write(t.site("data/media/a.txt"), b"newer").expect("write");
    t.units.set_active(&format!("setu-site@{S}.service"), true);
    let err = t
        .run_io(Verb::Restore, json!({"dirs": ["media"]}), &archive)
        .expect_err("active unit");
    assert!(matches!(err, HelperError::Refused(_)), "{err:?}");
    assert_eq!(
        std::fs::read(t.site("data/media/a.txt")).expect("read"),
        b"newer"
    );
    assert_eq!(names(&t.site("")), ["bin", "data", "run"]);
    // Stopped: the same restore goes through.
    t.units.set_active(&format!("setu-site@{S}.service"), false);
    t.run_io(Verb::Restore, json!({"dirs": ["media"]}), &archive)
        .expect("restore");
    assert_eq!(
        std::fs::read(t.site("data/media/a.txt")).expect("read"),
        b"live"
    );
}

#[test]
fn restore_rejects_hostile_tar() {
    let t = setup();
    t.create(&["media"]);
    std::fs::write(t.site("data/media/a.txt"), b"keep").expect("write");
    let before = snapshot(t.root());
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("outside dirs", tar_of(&[file("other/x", 0o644, b"x")])),
        ("top-level file", tar_of(&[file("x", 0o644, b"x")])),
        (
            "dotdot",
            tar_of(&[file("media/../../../escaped", 0o644, b"x")]),
        ),
        ("absolute", tar_of(&[file("/media/x", 0o644, b"x")])),
        (
            "symlink",
            tar_of(&[
                dir_e("media/"),
                E {
                    name: b"media/link",
                    kind: tar::EntryType::Symlink,
                    mode: 0o777,
                    data: b"",
                    link: Some("/etc/shadow"),
                },
            ]),
        ),
        (
            "hardlink",
            tar_of(&[
                file("media/a", 0o644, b"a"),
                E {
                    name: b"media/h",
                    kind: tar::EntryType::Link,
                    mode: 0o644,
                    data: b"",
                    link: Some("/etc/shadow"),
                },
            ]),
        ),
        ("setuid", tar_of(&[file("media/x", 0o4755, b"x")])),
        (
            "device",
            tar_of(&[E {
                name: b"media/dev",
                kind: tar::EntryType::Char,
                mode: 0o644,
                data: b"",
                link: None,
            }]),
        ),
    ];
    for (case, archive) in cases {
        let err = t
            .run_io(Verb::Restore, json!({"dirs": ["media"]}), &zst(&archive))
            .expect_err(case);
        assert!(matches!(err, HelperError::Refused(_)), "{case}: {err:?}");
        assert_eq!(
            snapshot(t.root()),
            before,
            "{case}: nothing changed, no staging left"
        );
    }
    // Not zstd at all.
    t.run_io(Verb::Restore, json!({"dirs": ["media"]}), b"garbage")
        .expect_err("garbage");
    assert_eq!(snapshot(t.root()), before);
}

#[test]
fn site_remove_removes_tree() {
    let t = setup();
    t.create(&["media"]);
    t.run(Verb::WriteUnit, S, unit_input(&[])).expect("unit");
    t.run(Verb::WriteEnv, S, json!([{"name": "A", "value": "b"}]))
        .expect("env");
    t.run(Verb::Unit, S, json!({"op": "start"})).expect("start");
    let outside = t.root().join("outside");
    std::fs::create_dir_all(&outside).expect("mkdir");
    std::fs::write(outside.join("keep"), b"k").expect("write");
    std::os::unix::fs::symlink(&outside, t.site("data/media/link")).expect("symlink");
    let reloads = t.units.reloads();

    t.run(Verb::SiteRemove, S, json!({})).expect("remove");
    assert!(!t.site("").exists(), "site tree gone");
    assert!(
        !dropin_path(&t).parent().expect("dir").exists(),
        "drop-in gone"
    );
    assert!(t.users.get(S).is_none(), "user gone");
    assert!(t
        .units
        .calls()
        .contains(&format!("stop setu-site@{S}.service")));
    assert_eq!(t.units.reloads(), reloads + 1);
    assert!(outside.join("keep").is_file(), "symlink target untouched");
    // Idempotent.
    t.run(Verb::SiteRemove, S, json!({})).expect("again");
}

#[test]
fn unit_show_reports_props_and_data_bytes() {
    let t = setup();
    t.create(&["media"]);
    std::fs::write(t.site("data/media/a.bin"), vec![0u8; 4096]).expect("write");
    t.run(Verb::Unit, S, json!({"op": "start"})).expect("start");
    let show = t.run(Verb::Unit, S, json!({"op": "show"})).expect("show");
    assert_eq!(show["ActiveState"], "active");
    assert_eq!(
        show["ControlGroup"],
        format!("/system.slice/system-setu\\x2dsite.slice/setu-site@{S}.service")
    );
    assert!(
        show["data_bytes"].as_u64().expect("bytes") >= 4096,
        "{show}"
    );
    let err = t
        .run(Verb::Unit, S, json!({"op": "reboot"}))
        .expect_err("bad op");
    assert!(matches!(err, HelperError::Invalid(_)));
}

/// Paths of the regular files in a backup archive.
fn archived_files(archive: &[u8]) -> Vec<String> {
    let dec = zstd::stream::read::Decoder::new(archive).expect("zstd");
    let mut ar = tar::Archive::new(dec);
    let mut out: Vec<String> = ar
        .entries()
        .expect("entries")
        .map(|e| e.expect("entry"))
        .filter(|e| e.header().entry_type() == tar::EntryType::Regular)
        .map(|e| e.path().expect("path").display().to_string())
        .collect();
    out.sort();
    out
}

/// Backup runs as root over a tree the site user controls: a hard link
/// to a file the site user cannot read (root's, another site's) or a
/// file it does not own is skipped, so a backup cannot exfiltrate it
/// (M4).
#[test]
fn backup_skips_foreign_owned_and_hardlinked_files() {
    let t = setup();
    t.create(&["media"]);
    let me = nix::unistd::getuid().as_raw();
    t.users.set(S, me, me);
    std::fs::write(t.site("data/media/a.txt"), b"mine").expect("write");
    std::fs::create_dir_all(t.site("data/media/sub")).expect("mkdir");
    std::fs::write(t.site("data/media/sub/b.txt"), b"mine too").expect("write");
    let secret = t.root().join("secret");
    std::fs::write(&secret, b"root only").expect("write");
    std::fs::hard_link(&secret, t.site("data/media/h.txt")).expect("hard link");

    let (_, archive) = t
        .run_io(Verb::Backup, json!({"dirs": ["media"]}), b"")
        .expect("backup");
    assert_eq!(
        archived_files(&archive),
        ["media/a.txt", "media/sub/b.txt"],
        "the hard link is skipped"
    );

    // Owned by someone else than the site user: skipped too.
    t.users.set(S, me + 1, me + 1);
    let (_, archive) = t
        .run_io(Verb::Backup, json!({"dirs": ["media"]}), b"")
        .expect("backup");
    assert!(
        archived_files(&archive).is_empty(),
        "foreign-owned files skipped"
    );
}
