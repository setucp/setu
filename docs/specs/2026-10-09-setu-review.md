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

## Items the reviewer could not verify — checked 2026-10-09

| Item | Result | Effect |
|---|---|---|
| `jrei/systemd-ubuntu:24.04` exists | yes (`docker manifest inspect`) | e2e image confirmed; GitHub-runner behaviour under `cgroupns=private` still to be seen in CI |
| `tower-sessions-sqlx-store` with sqlx 0.9 | no: 0.15.0 depends on sqlx ^0.8 | own `sessions` table, decided in spec §11 and Task 7 |
| musl build of rustls | use the `ring` provider (no cmake) | pinned in the Tech Stack line |
| Caddy config persistence | the apt package ships `caddy-api.service` (`caddy run --environ --resume`) for API-driven config | agent install uses it with `--config /etc/caddy/setu.json --resume`; e2e assertion 12 covers restart and autosave loss |
| 60-second provisioning on real hardware | unverified | e2e prints the time; Task 15 records it on a Hetzner box if the owner provides one |

## Second pass (revision 2 → revision 3)

A second independent reviewer checked revision 2 against the first
pass's claimed resolutions and found the gaps below. All are resolved
in spec revision 3 and plan revision 3.

| # | Finding | Resolution (rev 3) |
|---|---|---|
| 2-C1 | Update never wrote the new version's env; e2e versions differed only by env | `env` step (`resolve::merge`) and `write_env` in the update path; old env and spec sealed in the job payload; transcript test |
| 2-C2 | Rollback proof broken: `pg_restore --clean` keeps objects the failed version created; hello's failing version added no migration | Restore **replaces** the database (`DROP … WITH (FORCE)` → create → `pg_restore`); dirs restored by staging and swap; hello 0.1.2 migrates (`notes_v3`) then fails, 0.1.1 refuses to boot if `notes_v3` exists |
| 2-C3 | Helper still accepted path-like input (`dest`, `src`, `version`, `url`, raw `exec` into `ExecStart`), `--root` reachable via sudo, `install-system` sudo-reachable | Verb table rewritten: backup/restore stream tar over stdout/stdin; `install {version, sha256}` reads a derived download path; helper validates version/exec/args/dirs with the shared regexes and systemd-escapes; `--root` only under feature `test-root` and refused with `SUDO_UID`; `install_system` is a library function run as real root, not a `helper` verb |
| 2-C4 | Postgres password on argv (`-v pw=`) | `\set pw '…'` as the first stdin line; `pg_dump` via `PGPASSWORD`; installer writes the password into the config file via heredoc |
| 2-I5 | Restore extracted as root into a user-owned, possibly live tree; backup files under a setu-owned dir could be symlinks | Restore requires the unit inactive, extracts into root-owned staging with the same entry rules as install, then swaps; helper reads archives with `O_NOFOLLOW` and requires regular files |
| 2-I6 | Join retry by hostname let a bystander hijack the server | Agent nonce; same nonce → same sealed token within 60 s; different nonce refused |
| 2-I7 | Caddy `PUT /id/…` inserts a duplicate | `PATCH` for existing, `POST …/routes` for new; fake rejects `PUT` |
| 2-I8 | `caddy-api.service` needs `RuntimeDirectory`, `Group`, `ExecStart=` reset; `caddy.service` must be stopped; `--resume` ignores base changes on reinstall; panel route not reconciled | All in spec §4 Caddy and Task 10b assets; re-run applies the base via `POST /load` merged with current routes; panel route in `agent.toml`, id `setu-panel`, reconciled |
| 2-I9 | Single-node join under `tls internal` fails (internal CA, name resolution) | `--control-plane http://127.0.0.1:<port>` on TCP installs; loopback http allowed without `--insecure`; `--ca` for the internal CA on unix-socket installs; test-install adds `setu.test` to the container's hosts |
| 2-I10 | Serial agent loop | Spawn per command with per-site FIFO; read-only commands take no slot; test |
| 2-I11 | Backups: 2-minute TTL, no file deletion, `pre_restore` missing, paths keyed by timestamp | 60-minute TTL; `DeleteBackup` command and `delete_backup` jobs from the scheduler; `pre_restore` kind; directories keyed by `backup_id` |
| 2-I12 | Secrets survive cancel/expire/requeue; `Join*`/`Config` printable | Redact on cancel, expiry and prune; redacting `Debug` on `JoinRequest`, `JoinResponse`, `Config`, agent config; app-log caveat documented |
| 2-I13 | Vyasa `workdir = "bin"` is the archive root, not the binary's directory | `workdir = "exec_dir"` (default) |
| 2-I14 | Hosted apps could forge `X-Forwarded-For` to the panel on 127.0.0.1 | Panel listens on a group-owned unix socket behind local Caddy; forwarded headers trusted from unix peers only; Vyasa cross-site caveat in SECURITY.md |
| 2-I15 | e2e assertions vacuous or looping (firewall, agent exit drop-in, feature flag, dedupe semantics, reconcile knob) | Assertion 4 uses a `0.0.0.0` listener; the exit hook is a cargo feature; the drop-in is removed before restart; dedupe semantics fixed (persist before post, cached result on redelivery); `SETU_AGENT_RECONCILE_SECS` in Task 10d |
| 2-I16 | Helper tests asserted ownership unprivileged | `Ownership` and `Users` traits with recording fakes; e2e asserts real `stat` output |
| 2-I17 | Sessions table unowned and in the wrong crate | `sessions` in migration 0001 with repo in `db`; purge by the scheduler; cascade and delete-on-password-change |
| 2-I18 | Consumed-but-unproduced interfaces (disk bytes, dirs at site-create, control-plane flag, setup token producer, notifier, systemd trait, site metrics, env job kind) | `unit show` returns `data_bytes`; `site-create {dirs}`; `servers.is_control_plane`; `serve` generates the setup token; notifier in Task 8; reconcile uses the helper; `GET /sites/{id}` includes metrics; `env_update` job |
| 2-I19 | nftables persistence; `flush ruleset` danger; host port check | `setu-nftables.service` loading only `table inet setu`; port-free check in `validate` |
| 2-I20 | LISTEN/NOTIFY: dropping test DBs with open listeners, subscribe-before-check, reconnect gaps, stale compiled migrations | `DROP … WITH (FORCE)`; subscribe before `next_for`; 5 s fallback re-check; `build.rs` rerun-if-changed |
| 2-I21 | Capacity formula left no OS headroom; retry double-counted | 85 % cap; "other sites" + this site |

Minors from the second pass applied: setuid entries refuse the whole
install; `Health{site}` only; reveal audited per call; alphanumeric
`password20`; once/health app contract documented; lock literal
`0x5e7a`; install semaphore tested; agent unit without
`NoNewPrivileges`; apt installs `sudo gnupg`; every task has a run
command and an expected count; scheduler and join tests use an
injected clock or a direct `UPDATE`; e2e on a dedicated Docker
network; Vyasa provisioning held to 60 s; CLAUDE.md remote and layout
lines corrected; ui-implementer points at §7; skills corrected.

Still unverified (for the implementing session): Vyasa 0.2.0's login
route and any Origin requirement (e2e assertion 12 sends `Origin`);
`visudo -cf` ownership checks on a non-root-owned file (Task 10b
ledgers the outcome); Caddy admin `Host` handling over a unix socket;
GitHub-runner behaviour with `--cgroupns=private`.
