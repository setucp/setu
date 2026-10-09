---
name: setu-testing
description: How to run Setu's Rust and UI tests safely on this host — throwaway Postgres container, ports, sqlx offline data, single heavy cargo job. Use before running any test or build.
---

# Testing Setu

1. `df -h /` — stop if free space < 20 GB; `cargo clean -p <crate>` or
   remove `target/debug/incremental` first.
2. Postgres: `bash /home/gopal/Desktop/wp/setu/scripts/test-pg.sh up`
   (container `setu-test-pg`, port `SETU_TEST_PG_PORT`, default 55440;
   never 5432; if 55440 is busy export another port and keep using it
   for the session).
3. Rust: `CARGO_BUILD_JOBS=4 bash /home/gopal/Desktop/wp/setu/scripts/test.sh [-p crate] [name]`.
   Redirect long output to the scratchpad and read the tail.
4. After schema or query changes: `bash scripts/sqlx-prepare.sh`, commit `.sqlx/`.
5. UI: `pnpm -C /home/gopal/Desktop/wp/setu/ui lint && … typecheck && … test`.
   vitest is flaky under host load — rerun the single file before
   concluding.
6. OpenAPI snapshot: `UPDATE=1 scripts/test.sh -p setu-api openapi_snapshot`
   rewrites `openapi.json`; then `pnpm -C ui gen:api`.
7. Lints before every commit: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo deny check`.
