use std::fs;
use std::path::PathBuf;

#[test]
fn corpus() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
    let mut tomls: Vec<PathBuf> = fs::read_dir(&dir)
        .expect("read corpus dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "toml"))
        .collect();
    tomls.sort();
    assert!(!tomls.is_empty(), "no corpus files found");
    for toml_path in tomls {
        let stem = toml_path
            .file_stem()
            .expect("stem")
            .to_string_lossy()
            .to_string();
        let expect_path = dir.join(format!("{stem}.expect"));
        let src = fs::read_to_string(&toml_path).expect("read toml");
        let expect = fs::read_to_string(&expect_path)
            .unwrap_or_else(|_| panic!("missing .expect for {stem}"))
            .trim()
            .to_string();
        let result = setu_manifest::Manifest::parse(&src);
        if expect == "ok" {
            match result {
                Ok(_) => {}
                Err(errs) => panic!("{stem}: expected ok, got errors: {errs:?}"),
            }
        } else {
            match result {
                Ok(_) => panic!("{stem}: expected error {expect:?}, got ok"),
                Err(errs) => {
                    assert_eq!(
                        errs.len(),
                        1,
                        "{stem}: expected exactly one error line, got {errs:?}"
                    );
                    let actual = format!("error: {}", errs[0]);
                    assert_eq!(actual, expect, "{stem}: error mismatch");
                }
            }
        }
    }
}
