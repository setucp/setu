//! Build script: detect a prebuilt UI so `embed.rs` knows whether it can
//! serve the real bundle.
//!
//! `scripts/build-ui.sh` must run before `cargo build` for release/serve
//! builds; when `ui/dist/index.html` is absent the binary still compiles
//! (and `cargo test` never needs the UI) but serves API-only with a
//! warning. Failing the build here would also break `cargo test` without
//! the UI, so absence is a warning, not an error.

use std::path::PathBuf;

fn main() {
    println!("cargo::rerun-if-changed=../../ui/dist/index.html");
    println!("cargo::rustc-check-cfg=cfg(setu_ui)");
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if manifest.join("../../ui/dist/index.html").exists() {
        println!("cargo::rustc-cfg=setu_ui");
    } else {
        println!(
            "cargo::warning=ui/dist/index.html not found; building API-only \
             (run scripts/build-ui.sh first for the embedded UI)"
        );
    }
}
