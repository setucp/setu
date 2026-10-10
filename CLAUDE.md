# Setu — notes for contributors and coding agents

Setu is a control panel for single-binary web apps: one binary `setu`
(`serve` = control plane with the embedded React UI, `agent` = per-server
agent, `helper` = the privileged helper reached only through `sudo -n`).
Design: `docs/specs/2026-10-09-setu-design.md`; review record:
`docs/specs/2026-10-09-setu-review.md`; everything else under `docs/`.

- Gate before every commit: `cargo fmt --check && cargo clippy
  --workspace --all-targets --all-features -- -D warnings && bash
  scripts/test.sh`, plus `pnpm -C ui lint && pnpm -C ui typecheck && pnpm
  -C ui test` when `ui/` changed. `scripts/test.sh` starts a throwaway
  Postgres container (`SETU_TEST_PG_PORT`, default 55440); never point
  tests at a real database.
- Security invariants (see `docs/SECURITY.md`): nothing path-like, no
  mode and no owner crosses the sudo boundary; the helper takes a short
  id and typed JSON only and re-validates everything; secrets never on
  argv, in logs or in `Debug`; every agent command is idempotent.
- Migrations are forward-only (`crates/db/migrations`); regenerate
  `.sqlx/` with `scripts/sqlx-prepare.sh`. UI types come from the
  checked-in `openapi.json` (`pnpm -C ui gen:api`), never a server.
- End-to-end proof: `scripts/e2e.sh` (14 assertions in a systemd
  container) and `scripts/test-install.sh` (the installer); see
  `docs/E2E.md`.
- No AI co-author trailers in commits.
