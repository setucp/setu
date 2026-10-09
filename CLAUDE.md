# Setu — working rules for agents

Setu is a control panel for single-binary web apps (and, from phase 2,
container images): control plane `setu`, per-server `setu-agent`, React
UI embedded in the control-plane binary. Read `docs/HANDOFF.md`, then the spec
`docs/specs/2026-10-09-setu-design.md` and the plan
`docs/plans/2026-10-09-setu-mvp.md`.
The spec is the authority; the plan argues from it; disagreements are
rulings recorded in the plan's ledger.

## Non-negotiable

- No Claude co-author trailers, no `Claude-Session:` lines, no
  "Generated with Claude Code" in commits or PRs.
- Remote is github.com/vyasa-cms/setu (private). Never make it
  public. Never force-push shared branches. Never bare `git stash`.
- The host runs production services as this user: never `pkill` by
  name; stop only processes whose PID you saved. Never connect to
  Postgres on port 5432; tests use the throwaway container from
  `scripts/test-pg.sh` on `SETU_TEST_PG_PORT` (default 55440).
- One heavy cargo job at a time, `CARGO_BUILD_JOBS=4`. Check `df -h /`
  before long builds (`target/` has filled the disk before).
- Absolute paths in every shell command; parallel calls share a cwd.
- Never print secrets (tokens, env values, keys). Structs holding them
  get a manual `Debug` that redacts.
- UI codegen (`pnpm -C ui gen:api`) reads `openapi.json` from the repo,
  never a running server.

## Workflow

- superpowers skills: brainstorming → writing-plans →
  subagent-driven-development (or executing-plans) → requesting-code-
  review → finishing-a-development-branch. TDD on every task: a test you
  watched fail, then the code.
- Work in a worktree (`../setu-work-<n>`), one branch per plan. Squash-
  merge to `main` locally until a remote exists.
- Repo agents in `.claude/agents/` (`rust-implementer`, `ui-implementer`,
  `code-reviewer`, `e2e-verifier`) and skills in `.claude/skills/`
  (`setu-testing`, `setu-e2e`, `setu-conventions`) — use them.
- Every task ends green:
  `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && scripts/test.sh`,
  plus `pnpm -C ui lint && pnpm -C ui typecheck && pnpm -C ui test` when `ui/` changed.
- Migrations are forward-only sqlx files in `crates/db/migrations`;
  `.sqlx/` offline data is regenerated with `scripts/sqlx-prepare.sh`.

## Layout

```
crates/common    config, sealing (v1: ChaCha20-Poly1305), ids, error envelope
crates/manifest  app manifest schema, validator corpus, env resolution
crates/proto     agent command/result/heartbeat types
crates/db        sqlx repos + migrations
crates/jobs      provision/update/backup/restore/delete state machines over AgentClient
crates/api       axum control plane, utoipa, embedded UI  → binary `setu`
crates/agent     Runtime trait, NativeRuntime, systemd/Postgres/Caddy adapters → `setu-agent`
crates/testkit   TestDb, TestServer, FakeAgent helpers
apps/hello       tiny Axum app used by tests and the e2e
ui/              Vite + React + TanStack + Tailwind + shadcn/ui
scripts/         test.sh, test-pg.sh, e2e.sh, build-ui.sh, sqlx-prepare.sh, check-sudoers.sh
```
