# Review of the Setu phase 1 spec and plan (2026-10-09)

An independent reviewer (fresh context, read-only) reviewed revision 1
of the spec and plan. Verdict: architecture sound; plan not executable
without security-critical guessing. Every Critical and Important
finding below was resolved in revision 2 of the spec and plan; the
resolution is recorded here so the reasoning survives.

## Critical

| # | Finding | Resolution (rev 2) |
|---|---|---|
| C1 | Privilege model unworkable and root-equivalent: no user creation/chown; agent cannot read site dirs for backup; `helper write-file` with free path/mode/owner and a prefix check is root via drop-ins, setuid or symlinks; bare `systemd-run` in sudoers; `setu-site@*` wildcard matches spaces | Typed helper verbs with JSON on stdin and paths derived from a validated short id (spec §4 table); root-owned site tree with the site user owning only `data/` and `run/`; env file root:root 0600; backups/restore/remove performed by the helper; sudoers is a golden file with no wildcards beyond `helper *`; `systemd-run` removed; journal via the `systemd-journal` group; helper test list; residual trust stated |
| C2 | Caddy admin API on TCP 2019 is reachable by every hosted app: any site can hijack every domain | Admin on a unix socket (0660, group `caddy`, `setu` in the group), TCP admin off, `doctor` asserts it; `REVOKE CONNECT … FROM PUBLIC` on every database |
| C3 | Vyasa binds `0.0.0.0:$PORT`: sites reachable on 20000–29999 bypassing Caddy | Env source `bind_addr` (`127.0.0.1:<port>`) used by the Vyasa manifest; nftables table dropping inbound 20000–29999; e2e asserts refusal |
| C4 | Vyasa cannot be provisioned: musl URL does not exist, archive has a top-level dir, admin assets resolved from cwd, `registry` dir unwritable, `VYASA_SITE_URL` does not exist, trusted proxies unset; e2e dropped Vyasa | Manifest gains `run.workdir` and `exec` as a path inside the archive; Vyasa manifest corrected (gnu target, `VYASA_REGISTRY_DIR`, `VYASA_TRUSTED_PROXIES`, no `SITE_URL`); e2e provisions real Vyasa 0.2.0 and logs in |
| C5 | Rollback after a migrating update leaves the old binary refusing to start (sqlx `VersionMissing`); rollback also ignored unit/env | Rollback restores the `pre_update` backup (database and dirs) and rewrites unit and env before starting the old version; `hello` 0.1.1 adds a migration so the e2e proves it |
| C6 | Control-plane restart strands `running` jobs forever; mutations 409 for good | Job leases (`locked_until`, renewed), reclaim on expiry, reset on boot, step cursor in the payload so resumed jobs continue; test with a killed runner |
| C7 | Single-node install unreachable (panel on 127.0.0.1, URL printed with the public IP), Postgres and Caddy installed too late, `/setup` hijackable over plain HTTP, owner creation racy, cookie Secure flag problem | `install.sh` installs Postgres and Caddy first and fronts the panel with Caddy (ACME or `tls internal`); one-time setup token; owner created under an advisory lock; Secure cookie when `public_url` is https |

## Important

| # | Finding | Resolution |
|---|---|---|
| I1 | Site user name > 32 chars; full vs short id contradiction | `short_id` (8 chars, unique per server) everywhere in user, unit, db and path names |
| I2 | Drop-in path wrong (template-wide `.service.d`); ExecStart cannot live in the template | Template holds hardening; per-instance drop-in holds ExecStart, WorkingDirectory, limits |
| I3 | Wrong cgroup path for metrics | Read `ControlGroup` from `systemctl show`; unit test on the builder; e2e asserts `memory_bytes > 0` |
| I4 | Caddy apt default config catches everything; routes added by API lost on restart; Ubuntu package old | Official Caddy repo; Setu-owned JSON config with `persist_config`; routes upserted by `@id`; reconcile on start and every 5 min |
| I5 | Commands outlive jobs and can run out of order; two retry mechanisms; generic timeouts | `expires_at` per kind; cancel on job timeout; one retry mechanism; agent dedupes by id and serialises per site; requeue on new `boot_id` |
| I6 | Port allocation race (lock released before insert); capacity overcommit | `sites.create` does lock, capacity, port, env, insert in one transaction |
| I7 | `Install` flipping `current` makes `Restart=on-failure` start the new version early | `install` never touches `current`; separate `switch_version` |
| I8 | Spec vs plan on status after rollback | Ruling: `running` on the old version, job `failed` with `rollback: true` |
| I9 | Secrets unsealed in `agent_commands`, echoed by psql errors, on argv | Secret fields nulled on completion; password via psql variable on stdin; agent-side redaction; test that logs never contain the password |
| I10 | Retry regenerates credentials | Env generated once at `POST /sites`; `create_database` is create-or-alter; domain check excludes self |
| I11 | No way to retrieve the once-secret | `POST /sites/{id}/env/{name}/reveal` with password re-auth, audited, UI test |
| I12 | Backups half-built: no scheduler, no restore in e2e, FK conflict on delete | Scheduler in the runner; e2e restore step; `backups.site_id ON DELETE SET NULL` with name/domain kept; `pg_restore --clean --if-exists --no-owner --role` |
| I13 | e2e topology: host cgroup bind mount endangers production; panel unreachable from the bridge; glibc mismatch | `--cgroupns=private`, no bind mount; serve bound to the bridge IP; musl required, target in `rust-toolchain.toml` |
| I14 | TLS untested; agent kill timing-dependent | `tls internal` with `curl -k --resolve`; `SETU_AGENT_TEST_EXIT_AFTER=<kind>` |
| I15 | `test.sh` argument order; config prefix separator; missing `url`; migrations dir missing in Task 1 | All fixed in Tasks 0–2 |
| I16 | `cargo deny` licence list | AGPL added to allow list; `[licenses.private] ignore`; `publish = false` |
| I17 | testkit ↔ api crate cycle | `TestServer` lives in `crates/api/tests/common/` and takes a `Router` |
| I18 | Dependency pins diverged from Vyasa; sealing scheme diverged | Pinned to Vyasa's current versions; KeyVault scheme (`v1:`/`plain:`, SHA-256 derivation with a label) copied |
| I19 | Task 10 too large | Split into 10a–10d with the missing tests |
| I20 | Log streaming through the queue floods it; no cursor; `Notify` cannot see enqueues from other crates | Journal cursor; read-only commands concurrent; pruning; `LISTEN/NOTIFY` wakes the long-poll |
| I21 | Login rate limit behind Caddy locks everyone | Trusted proxies setting; client IP from `X-Forwarded-For` only from them |

## Minor (all applied)

Domain normalisation specified; agent derives paths from the id only;
agent refuses `http://` without `--insecure`; join token on stdin/env;
join idempotent for 60 s; e2e manifests rendered per version; server
status computed only; `JobCtx` logging is async; `DbAgentClient`
defined once (Task 8); `setup/local-server` in Task 14's interfaces;
job kinds for restart/stop/start; `setu backup` in Task 14; TOTP cut;
disk quota reported only; `metrics.prune` called by the scheduler;
rust-embed `allow_missing`; CI uses `test-pg.sh`; task count fixed;
skeleton file list corrected; clippy block copied; package names
declared; operator password on stdin; capacity rule includes overhead.

## Scope cuts accepted

Container image of Setu, aarch64, Swagger UI, TOTP, operator tokens and
CLI, audit UI page, metric charts, fleet rollout batches,
`update.strategy = "command"`, enforced disk quotas.

## Declined to judge (left for the implementing session to verify)

`jrei/systemd-ubuntu:24.04` under `cgroupns=private` on GitHub runners;
musl build of the rustls provider; `tower-sessions-sqlx-store` with
sqlx 0.9 (fallback: own session table); Caddy `persist_config`
behaviour in the current package; the 60-second target on real
hardware.
