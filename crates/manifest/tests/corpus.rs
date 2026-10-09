//! Validator corpus (plan Task 3): every `*.toml` has a `*.expect` holding
//! `ok` or the single `error: <message> line <n>` the validator reports.

use std::fs;
use std::path::PathBuf;

const CASES: usize = 17;

#[test]
fn corpus() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
    let mut tomls: Vec<PathBuf> = fs::read_dir(&dir)
        .expect("read corpus dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "toml"))
        .collect();
    tomls.sort();
    assert_eq!(tomls.len(), CASES, "corpus case count");
    let mut failures = Vec::new();
    for toml_path in tomls {
        let stem = toml_path
            .file_stem()
            .expect("stem")
            .to_string_lossy()
            .to_string();
        let src = fs::read_to_string(&toml_path).expect("read toml");
        let expect = fs::read_to_string(dir.join(format!("{stem}.expect")))
            .unwrap_or_else(|_| panic!("missing .expect for {stem}"))
            .trim()
            .to_string();
        let actual = match setu_manifest::Manifest::parse(&src) {
            Ok(m) => {
                // Stored manifests are JSON: they must read back.
                let json = serde_json::to_value(&m).expect("manifest serializes");
                let back: setu_manifest::Manifest =
                    serde_json::from_value(json).expect("stored manifest reads back");
                assert_eq!(back.app.id, m.app.id);
                "ok".to_string()
            }
            Err(errs) if errs.len() == 1 => format!("error: {}", errs[0]),
            Err(errs) => format!("{} errors: {errs:?}", errs.len()),
        };
        if actual != expect {
            failures.push(format!("{stem}:\n  expected {expect}\n  actual   {actual}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// `apps/hello/manifest.tmpl.toml` is the template `scripts/e2e.sh`
/// renders per version: each rendering must validate under the current
/// schema.
#[test]
fn hello_template_renders_valid() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apps/hello/manifest.tmpl.toml");
    let template = fs::read_to_string(path).expect("read hello template");
    for (version, extra) in [
        ("0.1.0", ""),
        ("0.1.1", ""),
        ("0.1.2", "HELLO_FAIL = { value = \"1\" }"),
    ] {
        let src = template
            .replace("__VERSION__", version)
            .replace("__EXTRA_ENV__", extra)
            .replace("__SHA256__", &"a".repeat(64))
            .replace("__URL__", "http://127.0.0.1:1/hello.tar.gz");
        let manifest = setu_manifest::Manifest::parse(&src)
            .unwrap_or_else(|e| panic!("hello {version} template validates: {e:?}"));
        assert_eq!(manifest.app.version, version);
        for var in [
            "HELLO_BIND",
            "DATABASE_URL",
            "HELLO_VERSION",
            "HELLO_UPLOADS_DIR",
            "HELLO_STATE_DIR",
        ] {
            assert!(manifest.env.contains_key(var), "{version}: {var}");
        }
        assert_eq!(manifest.env.contains_key("HELLO_FAIL"), version == "0.1.2");
    }
}
