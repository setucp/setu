# Changelog

All notable changes to Setu. Release notes are cut from the section
matching the tag.

## 0.1.0 — Unreleased

First release (MVP): one binary (`setu serve` control plane +
`setu agent` per server), single-server native runtime on Ubuntu
24.04, REST API + embedded React UI, first-owner setup wizard
(`GET/POST /api/v1/setup`, `POST /api/v1/setup/local-server`),
`install.sh` single-node installer, app manifests with validation
corpus, sites with Postgres/Caddy/TLS/limits, local backups,
updates with rollback, job runner, audit log, end-to-end test.
Caddy is driven only through its admin unix socket (TCP admin off,
`caddy-api.service` with `/etc/caddy/setu.json`), and the `inet setu`
nftables table keeps site ports 20000–29999 off the network.
The agent runs commands concurrently (per-site FIFO, read-only
commands unslotted), rejects expired ones, dedupes by id and persists
each result before posting it; it reconciles Caddy routes (sites and
panel) from its state every 5 minutes. `setu agent install` reads the
join token from `SETU_JOIN_TOKEN` or stdin (never argv), keeps one join
nonce across retries, accepts `--insecure`/`--ca`, and creates
`agent.toml` 0600 from the start; `setu agent doctor` checks the
helper, sudoers, units, Caddy, firewall, Postgres and the control
plane.
`setu serve` listens on TCP or a unix socket (`listen =
"unix:/run/setu/http.sock"`, 0660, group `caddy`), writes a one-time
setup token to `/var/lib/setu/setup-token` on first boot (the owner
form requires it), keeps sessions in its own table (cookie
`setu_session`, sliding 7 days, ended on logout and password change),
refuses cross-origin mutations (`Origin` must be the panel URL, or
`Sec-Fetch-Site: same-origin`), takes the client IP from
`X-Forwarded-For` only behind its unix socket or a trusted proxy, and
wakes agent long polls with Postgres LISTEN/NOTIFY. New routes: env
reveal with password re-entry (audited), backup delete, server cordon,
password change; `GET /sites/{id}` carries the latest metric sample and
servers their latest load. No Swagger UI; the OpenAPI document is at
`/api/v1/openapi.json` for logged-in operators. `setu operator create`
reads the password from `SETU_OPERATOR_PASSWORD` or stdin; `setu backup
--out <file>` dumps the control plane's database.
`install.sh` (root, Ubuntu 24.04 x86_64; `--panel-host <dns>` and/or
`--tls internal`, `--version`) verifies the release archive against
`SHA256SUMS`, installs `/opt/setu/setu`, PostgreSQL 16 and (through
`setu agent install-system --caddy-only`, which now installs the
`caddy` package when missing) Caddy, creates the `setu` role and
database, writes `/etc/setu/setu.toml` 0600 with `listen =
"unix:/run/setu/http.sock"`, starts `setu.service`, waits for
`/healthz` through Caddy and prints the setup URL and token; re-runs
keep the config. `setu setting set tls_internal true|false` records the
panel's TLS mode; the wizard's local-server command then carries
`--panel-host` and `--tls internal --ca …`. Releases are x86_64 musl
only: one archive, `install.sh` and `SHA256SUMS` per GitHub release
(no aarch64, no container image until phase 2).
