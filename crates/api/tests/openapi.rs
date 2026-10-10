//! OpenAPI snapshot: the served document must equal the checked-in
//! `openapi.json` at the workspace root (`UPDATE=1` rewrites it; the UI
//! codegen reads that file, never a running server).

#[test]
fn openapi_snapshot() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../openapi.json");
    let generated = setu_api::rest::docs::openapi_json_string();
    if std::env::var("UPDATE").as_deref() == Ok("1") {
        std::fs::write(path, &generated).expect("rewrite openapi.json");
        return;
    }
    let checked_in = std::fs::read_to_string(path)
        .unwrap_or_else(|_| panic!("{path} missing: run UPDATE=1 to generate it, then commit it"));
    assert_eq!(
        checked_in, generated,
        "openapi.json drifted: run UPDATE=1 bash scripts/test.sh -p setu-api --test openapi"
    );
}
