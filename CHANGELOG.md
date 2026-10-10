# Changelog

All notable changes to Setu. Release notes are cut from the section
matching the tag.

## 0.1.0 — 2026-10-10

First release (MVP): a control panel for single-binary web apps on
Ubuntu 24.04 x86_64 servers you own. Design: spec revision 4
(`docs/specs/2026-10-09-setu-design.md`).

### Fixed before release

- `install_system` now sets `caddy:caddy 0750` on `/var/lib/caddy` with
  `install -d`, so a pre-existing directory (a mounted volume, a
  re-install) no longer leaves Caddy unable to write its internal CA
  and autosave (found deploying the panel into a container).

### The binary

- One binary, `setu`: `serve` (control plane), `agent
  run|install|install-system|join|doctor`, `helper <verb>` (the
  privileged helper, run only through `sudo -n`), `migrate`, `operator
  create`, `backup`, `setting set`, `openapi`.
- Releases are x86_64 musl static only: one archive, `install.sh` and
  `SHA256SUMS` per GitHub release, built without test features. No
  aarch64 build and no container image until phase 2.

### Control plane

- REST API under `/api/v1` with an embedded React UI: setup wizard,
  sites, servers, apps, plans, jobs, settings. The OpenAPI document is
  at `/api/v1/openapi.json` for logged-in operators; there is no
  Swagger UI.
- One-time setup token: written to `/var/lib/setu/setup-token` on the
  first boot without operators (again if the file is lost) and required
  to create the owner, exactly once.
- Operators with roles `owner` > `operator` > `viewer`, argon2id
  passwords, sessions in Postgres (cookie `setu_session`, sliding 7
  days, ended on logout and password change), a login rate limit of 10
  failures per IP per 15 minutes, and an audit row for every mutation.
- Every mutation, login and setup included, needs `Origin` equal to
  the panel URL or `Sec-Fetch-Site: same-origin`. The client IP comes
  from `X-Forwarded-For` only behind the unix socket or a trusted proxy.
- Listens on TCP or on a unix socket (`unix:/run/setu/http.sock`, 0660
  `setu:caddy`) behind Caddy.
- App catalogue from TOML manifests (validated with line numbers; by
  URL with a public-host guard). Plans with memory, CPU and disk
  limits.
- Sites: domain, port, database and secrets allocated in one
  transaction at creation; env generated exactly once and sealed
  (ChaCha20-Poly1305, Vyasa's KeyVault scheme); secrets masked, revealed
  only with the operator's password (audited, including denied
  attempts); env edits restart the site.
- Jobs with named, resumable steps and leases: provision (12 steps),
  update with automatic rollback (database restored when the new
  version ran), backup, restore (database replaced, dirs swapped),
  delete (after a `pre_delete` backup), start, stop, restart, env
  update, backup deletion. Job logs and agent commands never keep
  secrets.
- Scheduler: nightly backups at 03:00 with per-site jitter; scheduled
  copies beyond 7 and `pre_*` copies after 30 days deleted; metrics,
  commands and sessions pruned.
- Servers: join tokens with a nonce, cordon, latest load figures;
  sites show their latest CPU, memory and disk sample. Backups of
  deleted sites stay listed and deletable (`/api/v1/backups`).
- Agents long-poll for commands, woken by Postgres LISTEN/NOTIFY.
- `setu backup --out <file>` dumps the control plane's database
  (password in `pg_dump`'s environment); `setu operator create` reads
  the password from `SETU_OPERATOR_PASSWORD` or stdin; `setu setting
  set tls_internal true|false`.

### Agent

- Sites run as systemd units, one system user per site, hardened
  template unit plus a per-site drop-in with memory and CPU limits;
  start and stop also enable and disable, so state survives reboots.
- All root work goes through `setu helper` (nine typed verbs, JSON on
  stdin, paths derived from the site's short id, no-follow walks,
  hostile archives refused); the sudoers file is two lines.
- Postgres 16: one role and database per site, `REVOKE CONNECT … FROM
  PUBLIC`, passwords only on stdin; restore replaces the database.
- Caddy driven only through its admin unix socket (TCP admin off,
  `caddy-api.service` with `/etc/caddy/setu.json`), ACME or
  `--tls internal`; installed from Caddy's apt repository, or its
  checksum-pinned release package when the repository is unavailable.
- Firewall table `inet setu` keeps site ports 20000–29999 off the
  network.
- Commands run concurrently (per-site FIFO, read-only commands
  unslotted), expired ones are rejected, duplicates deduped, results
  persisted before posting; Caddy routes (sites and panel) reconciled
  from `state.json` every 5 minutes.
- `setu agent install` reads the join token from `SETU_JOIN_TOKEN` or
  stdin (never argv), keeps one join nonce across its retry, accepts
  `--insecure`, `--ca`, `--tls internal`, `--panel-host`, and creates
  `agent.toml` 0600 from the start; `setu agent doctor` checks eleven
  items.

### Installer

- `install.sh` (root, Ubuntu 24.04 x86_64; `--panel-host <dns>` and/or
  `--tls internal`, `--version`) verifies the release archive against
  `SHA256SUMS`, installs `/opt/setu/setu` (linked from
  `/usr/local/bin/setu`), PostgreSQL 16 and Caddy, creates the `setu`
  role and database, writes `/etc/setu/setu.toml` 0600 with the unix
  listener, starts `setu.service`, waits for `/healthz` through Caddy
  and prints the setup URL and token. Re-runs keep the config and
  restart nothing that did not change.
- The wizard's last step shows the command that joins the same box as
  a server (with `--panel-host` and, under `--tls internal`, the
  internal CA).

### Final review hardening

- A site domain equal to the panel host, `localhost` or a server's
  hostname is refused; the panel route is always Caddy's first route
  (install-system and reconcile).
- Log streams end when the client goes away, live at most 30 minutes
  and escape CR/LF; the job log tails until the job finishes and ends
  with `event: end`.
- Password attempts are counted before argon2 (which runs off the async
  workers); own password changes share the login limiter; `POST /setup`
  checks the token before hashing.
- Backups and restores stream between pipes and files; backup skips
  files the site user does not own or that are hard-linked; a failed
  restore swap puts the old data back; a restore whose `dirs.tar.zst`
  is missing fails; `delete_backup` serialises with the site's jobs.
- Only the job's lease holder writes its log, step and status.
- Postgres names are `site_<short>` only; database commands take the
  site's FIFO slot; downloads time out (10 s connect, 15 min total) and
  stale `.part` files are pruned at agent start; manifest URL fetches
  are pinned to the vetted addresses (NAT64 and 6to4 refused).
- `agent doctor` checks `fs.protected_hardlinks = 1`; `install.sh`
  restarts the local agent when an upgrade replaced the binary.

### Tests

- Unit and integration suites (manifest corpus, helper, golden units
  and sudoers, job state machines over a fake agent, every API route and
  role, OpenAPI snapshot), UI tests, an end-to-end test with real
  systemd, Postgres, Caddy, nftables and Vyasa 0.2.0 in a container (14
  assertions), and an installer test of the release build (9
  assertions).
