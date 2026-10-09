# Setu: a control panel for single-binary web apps

Date: 2026-10-09 · Status: approved in conversation (owner, 2026-10-09);
written for review · Working name: **Setu** (bridge). Rename freely; the
name appears only in binaries, crate names and the UI title.

## Intent

A hosting control panel for applications that ship as one binary (Rust,
Go, Zig, Bun-compiled, .NET single-file) or one container image. It does
for them what cPanel does for PHP: pick an app, get a domain, a database,
TLS, backups, updates and resource limits, from a web UI, in under a
minute, on servers you own. Vyasa is the first app in the catalogue and
the reference customer; the owner will sell Vyasa hosting plans on it.

Two audiences, in order: (1) the owner operating a fleet of Hetzner
boxes selling plans; (2) self-hosters installing Setu on one box. The
design serves (1) first and never does anything that blocks (2).

Success for the MVP (this spec's phase 1): on a fresh Ubuntu 24.04 VPS,
`curl …/install.sh | sh` installs Setu; the browser wizard creates the
first operator; "Add server" on the same box registers it; "New site →
Vyasa → example.com" yields a working Vyasa with TLS, Postgres and
backups, in under 60 seconds, with limits enforced; "Update" moves it
to a newer Vyasa and rolls back on a failed health check; the whole
suite is tested, including an end-to-end run against a real systemd.

Not in phase 1, decided: billing and customers (phase 3), containers as
a runtime (phase 2, but the manifest and supervisor contract carry it
from day one), multi-server placement (phase 3; the data model carries
`server_id` from day one), building from source, managed runtimes (PHP,
Node, Python: never — apps needing an interpreter ship as an image).

## 1. Shape

One binary, `setu`, from one Cargo workspace and one repository, with
three roles selected by subcommand (the k3s model: `k3s server` /
`k3s agent`):

- **`setu serve`** — the control plane. One process: REST API,
  embedded web UI (rust-embed), job runner, Postgres for its own
  state. Runs under systemd (`setu.service`) on any box, usually its
  own small one. Listens on `127.0.0.1:7000` by default; a Caddy in
  front terminates TLS.
- **`setu agent run|install|join|doctor`** — one per managed server
  (`setu-agent.service`). Supervises sites as systemd units, manages
  the local Postgres, writes Caddy's config through its admin API,
  streams logs and metrics. Stateless beyond a small `state.json`; the
  control plane is the source of truth. Talks outbound to the control
  plane over HTTPS with a per-server token (mTLS later). The control
  plane never SSHes.
- **`setu site|server|app|plan|job …`** — thin client over the REST
  API for scripting and the installer. Optional for the MVP; the
  installer can use `curl`.

Why one binary: the roles share most dependencies, the installer
downloads one archive, every server runs one version, and the single-
node case is `setu serve` plus `setu agent install` on the same file.
The cost is ~15 MB of unused control-plane code on each agent host.
The privileged file writer stays a fixed subcommand (`setu helper
write-file`) pinned by path in sudoers.

Why native systemd and not Docker: half the memory per site, instant
start, cgroups give CPU/memory/IO limits and accounting for free,
journald gives logs, and it matches how Vyasa already installs. Why an
agent and not SSH: SSH from a control plane means root keys on the
control plane and shell parsing; an agent has a typed API, runs as its
own user with scoped sudo rules, and reconciles state when the control
plane is down.

Runtime abstraction, fixed now so phase 2 does not rewrite the agent:

```rust
#[async_trait]
pub trait Runtime {
    async fn install(&self, site: &SiteSpec) -> Result<()>;   // fetch binary or image
    async fn write_unit(&self, site: &SiteSpec) -> Result<()>; // systemd unit or quadlet
    async fn start(&self, id: SiteId) -> Result<()>;
    async fn stop(&self, id: SiteId) -> Result<()>;
    async fn status(&self, id: SiteId) -> Result<RuntimeStatus>;
    async fn logs(&self, id: SiteId, since: Option<Cursor>) -> Result<LogStream>;
    async fn remove(&self, id: SiteId) -> Result<()>;
}
```

Phase 1 ships `NativeRuntime` (binary + systemd service). Phase 2 ships
`QuadletRuntime` (rootless Podman via systemd Quadlet) behind the same
trait; both produce a systemd unit, so limits, logs and status code is
shared.

## 2. App manifest

A TOML file per app version, published at a URL or pasted into the
catalogue. It is the only app-specific knowledge Setu has; the panel
never knows the language.

```toml
schema = 1

[app]
id = "vyasa"                 # [a-z0-9-], unique in the catalogue
name = "Vyasa"
version = "0.2.0"
homepage = "https://vyasa.site"
health = "/readyz"           # GET → 2xx means ready
port_env = "PORT"            # app reads its listen port here
admin_path = "/admin"        # link shown after provisioning

[run]
kind = "binary"              # "binary" | "image"
[run.binary]
"x86_64-linux" = { url = "https://github.com/vyasa-cms/vyasa/releases/download/v0.2.0/vyasa-0.2.0-x86_64-unknown-linux-musl.tar.gz", sha256 = "…", exec = "vyasa" }
"aarch64-linux" = { url = "…", sha256 = "…", exec = "vyasa" }
args = ["serve"]
# [run.image]  image = "ghcr.io/vyasa-cms/vyasa:0.2.0"  (phase 2)

[needs]
postgres = ">=16"            # one database + role per site
dirs = ["media", "index"]    # persistent, under the site's data dir, backed up

[env]                        # value spec per variable
VYASA_SECRET_KEY = { generate = "hex32", secret = true }
VYASA_DATABASE_URL = { from = "postgres_url" }
VYASA_MEDIA_DIR = { from = "dir", dir = "media" }
VYASA_INDEX_DIR = { from = "dir", dir = "index" }
VYASA_RUN_DIR = { from = "run_dir" }
VYASA_SITE_URL = { from = "site_url" }
VYASA_ADMIN_EMAIL = { from = "owner_email" }
VYASA_ADMIN_PASSWORD = { generate = "password20", secret = true, once = true }

[limits.default]             # overridden by the plan
memory_mb = 512
cpu_percent = 100            # 100 = one core
disk_mb = 5120

[backup]
postgres = true              # pg_dump the site's database
dirs = ["media"]             # "index" is derivable, excluded by default

[update]
strategy = "replace"         # fetch new binary, stop, swap, start, health; rollback on failure
# strategy = "command", command = "vyasa update apply"   (apps that self-update)
```

Rules: every `env` entry is `generate`, `from`, `value` or `prompt`
(asked at site creation, with `secret` and `once` modifiers); `once`
means written on first boot only and deleted from the env file after
the first successful health check. Unknown keys are a validation error
with a line number. The manifest validator is a pure function with its
own test corpus (`crates/manifest/tests/corpus/*.toml`, each with an
expected outcome).

## 3. Data model (control plane Postgres)

- `operators` (panel users): id, email, password_hash (argon2),
  totp_secret?, role (`owner` | `operator` | `viewer`), created_at.
- `servers`: id, name, host (public IP or hostname), token_hash,
  agent_version, arch, os, caddy_admin (bool), postgres_version,
  last_seen_at, capacity (cpu_cores, memory_mb, disk_mb), status
  (`pending` | `online` | `offline` | `draining`).
- `apps`: id (manifest `app.id`), name, homepage. `app_versions`:
  app_id, version, manifest (jsonb, validated), published_at,
  channel (`stable` | `beta`).
- `plans`: id, name, limits (memory_mb, cpu_percent, disk_mb,
  bandwidth_gb?), allowed_app_ids, created_at. Phase 1 has a seeded
  "Default" plan.
- `sites`: id, name, app_id, app_version, server_id, plan_id, domain,
  status (`provisioning` | `running` | `stopped` | `failed` |
  `updating` | `deleting`), port (assigned per server, 20000–29999),
  data_dir, db_name, env (jsonb, secrets sealed under
  `SETU_SECRET_KEY` with the same ChaCha20-Poly1305 `v1:` scheme as
  Vyasa's KeyVault), owner_email, created_at, updated_at.
- `jobs`: id, kind (`provision_site` | `update_site` | `backup_site`
  | `restore_site` | `delete_site` | `restart_site` | `reconcile`),
  site_id?, server_id?, status, attempts, log (text, appended),
  created_at, started_at, finished_at. The job runner is the same
  pattern as Vyasa's: Postgres queue, `FOR UPDATE SKIP LOCKED`.
- `backups`: id, site_id, taken_at, size_bytes, location (local path
  or `s3://…`), kind (`scheduled` | `manual` | `pre_update`), status.
- `audit_log`: operator_id, action, target, at, ip.
- `site_metrics` (phase 1 minimal): site_id, at, cpu_seconds,
  memory_bytes, disk_bytes, retained 30 days, 1-minute resolution.

Migrations are sqlx, forward-only, applied on boot under an advisory
lock, exactly as Vyasa.

## 4. Agent protocol

Outbound long-poll from the agent, so servers need no inbound port
besides 80/443: `GET /agent/v1/commands?wait=30s` with
`Authorization: Bearer <server token>` returns the next command;
`POST /agent/v1/commands/{id}/result` reports completion with a log
excerpt; `POST /agent/v1/heartbeat` every 15 s carries load, free
memory and disk, unit states and the 1-minute metrics batch. Commands
are JSON with a `kind` tag mirroring the `Runtime` trait plus
`create_database`, `drop_database`, `backup`, `restore`, `set_route`
(domain → port, for Caddy), `remove_route`, `fetch_logs`.

The agent is idempotent on every command (a repeated `write_unit`
rewrites the same file; a repeated `create_database` is a no-op) so the
control plane may retry freely. The agent keeps `/var/lib/setu/
state.json` (sites it knows, ports, db names) and on start reconciles
systemd and Postgres against it, then reports a full inventory.

Agent install (`setu agent install`, run by root once): create user
`setu`, directories `/var/lib/setu`, `/srv/setu/sites`, write
`/etc/sudoers.d/setu` allowing exactly `systemctl {start,stop,restart,
status,daemon-reload} setu-site@*`, `systemd-run`, and the unit-file
write path through a tiny setuid-free helper (`setu agent helper
write-unit`, invoked via sudo with a fixed path), ensure Postgres and
Caddy are installed (apt) with Caddy's admin API on
`127.0.0.1:2019`, write the agent's own systemd unit, join the control
plane with the one-time join token.

A site is `setu-site@<site-id>.service`, a template unit:

```ini
[Service]
User=setu-site-%i            # one system user per site
WorkingDirectory=/srv/setu/sites/%i
EnvironmentFile=/srv/setu/sites/%i/env
ExecStart=/srv/setu/sites/%i/bin/current/<exec> <args>
Restart=on-failure
MemoryMax=<limit>            # from the plan, via a drop-in
CPUQuota=<limit>%
ProtectSystem=strict
ReadWritePaths=/srv/setu/sites/%i/data /srv/setu/sites/%i/run
PrivateTmp=yes
NoNewPrivileges=yes
```

Limits live in a per-site drop-in so a plan change is a drop-in
rewrite plus `daemon-reload` and restart. Disk quota is enforced by
the agent (du-based check hourly in phase 1; project quotas on XFS in
phase 2) and shown in the UI.

## 5. Provisioning, update, backup, restore, delete

**Provision** (job `provision_site`), each step idempotent and logged:
1. Validate: domain is unique, plan allows the app, server is online
   with capacity (sum of plan memory ≤ server memory × 0.9).
2. Assign port, db name `site_<short id>`, generate env per manifest,
   seal secrets, insert the `sites` row as `provisioning`.
3. Agent: `create_database`, `install` (download + verify sha256 into
   `bin/<version>/`, symlink `bin/current`), `write_unit`, write env
   file (0600, owned by the site user), `start`.
4. Poll health through the agent (`GET http://127.0.0.1:<port><health>`)
   up to 90 s. On success: `set_route(domain → port)`; Caddy obtains
   the certificate; mark `running`; delete `once` secrets from the env
   file (the control plane keeps them sealed, shown once in the UI).
5. On failure anywhere: mark `failed` with the step name; leave the
   artefacts for inspection; "Retry" re-runs from the first step
   (idempotence makes this safe); "Delete" cleans up.

**Update** (job `update_site`, strategy `replace`): take a `pre_update`
backup; `install` the new version beside the old; stop; swap symlink;
start; health within 90 s; else swap back, start, mark `failed`
(`rollback: true`) and keep the site on the old version. Strategy
`command`: run the command as the site user, then health; no rollback
beyond what the app does itself. A fleet rollout is a job per site,
started in batches of N with a stop-on-first-failure switch.

**Backup** (job `backup_site`): `pg_dump -Fc` plus a tar of the
manifest's backup dirs, into `/var/lib/setu/backups/<site>/<ts>/`,
then optionally uploaded to S3-compatible storage configured on the
server (phase 1: local only, S3 upload in phase 2). Nightly schedule
per site, 7 local copies kept. **Restore** stops the site, restores
the dump into the same database and the dirs, starts, health.

**Delete**: stop, `remove_route`, `drop_database`, remove the site
user and directory after a final backup, delete the row. A 24-hour
"stopped, pending deletion" grace is phase 2.

## 6. REST API and UI

The API is `/api/v1`, JSON, session cookie for the UI and bearer
tokens (`operator_tokens`) for the CLI; utoipa generates the OpenAPI
document and the TS client, as in Vyasa. Resources: `auth`,
`operators`, `servers` (+ `join-token`), `apps` (+ versions, `validate`
takes a manifest and returns errors), `plans`, `sites` (+ `restart`,
`update`, `backups`, `restore`, `logs` as SSE, `env`), `jobs` (+ `log`
as SSE), `audit`.

UI (TanStack Router/Query/Table, Tailwind, shadcn/ui, same conventions
and lint config as Vyasa's admin):
- **Setup** wizard: first operator, panel URL, backup location.
- **Sites**: table (name, domain, app/version, server, plan, status,
  memory, disk); New site dialog (app → version → plan → server
  (auto) → domain → owner email → prompt-type env vars) → job page.
- **Site**: overview (status, URL, admin link, health), Logs (live),
  Env (masked, reveal with re-auth, edit → restart), Backups (list,
  take, restore), Updates (available versions, update, history),
  Limits (plan, current usage), Danger (stop, delete).
- **Servers**: list with heartbeat, load, sites, capacity bars; Add
  server shows the one-line install command with a join token; server
  page lists its sites and the agent log.
- **Apps**: catalogue with versions; Add app from URL or paste; the
  validator's errors inline.
- **Plans**: CRUD.
- **Jobs**: running and recent, live log.
- **Settings**: operators, 2FA, panel URL, backup defaults.

## 7. Security

Operator passwords argon2id; sessions tower-sessions in Postgres; TOTP
2FA optional in phase 1, enforceable per panel; CSRF by same-site
cookies plus origin check as in Vyasa. Server tokens 32 random bytes,
stored hashed; join tokens single-use, 1-hour expiry. Site secrets
sealed at rest with `SETU_SECRET_KEY` (required, 32 bytes hex, same
scheme as Vyasa's KeyVault). Each site runs as its own user with
`ProtectSystem=strict`, `NoNewPrivileges`, memory and CPU limits; the
agent never runs app code as `setu`. The agent's sudo surface is the
fixed command list above and is tested by a script that asserts the
sudoers file matches a golden file. Audit log on every mutating
operator action. Rate limits on login and on the agent command
endpoint. No telemetry.

## 8. Operations of Setu itself

`setu serve`, `setu migrate`, `setu operator create`, `setu backup`
(its own database), `setu agent install|join|run|doctor`. Config via
`setu.toml` and `SETU_*` env (bind addr, database URL, secret key,
public URL). `/healthz`, `/readyz`. Logs via tracing to journald.
Install script mirrors Vyasa's: downloads the checked archive for the
platform, installs to `/opt/setu`, writes the systemd unit, prints the
setup URL. Releases: GitHub Actions, musl static binaries for x86_64
and aarch64, checksums, a `setu` container image for people who prefer
it. Versioning semver, 0.1.0 is the MVP.

## 9. Testing

- Unit: manifest validator corpus; env generation; port allocation;
  sealing; job state machine (provision/update/rollback transitions as
  a pure function over a recorded agent transcript).
- API integration (`setu-testkit`, cloned from Vyasa's: throwaway
  Postgres per test on a non-production port): every endpoint, the
  authz matrix, OpenAPI snapshot.
- Agent: a `FakeSystemd`/`FakePostgres` behind traits for unit tests;
  then **one real end-to-end**: `scripts/e2e.sh` boots an Ubuntu
  24.04 container with systemd (`jrei/systemd-ubuntu` or an LXD/
  Incus container; a VM via `multipass` where available), installs
  the agent, provisions a tiny test app (a 200-line Axum binary built
  in-repo, `apps/hello`, with `/healthz`, a `PORT`, and a Postgres
  ping) and then Vyasa 0.2.0, asserts health, limits (`systemctl show
  MemoryMax`), a backup, an update to a deliberately broken binary
  that rolls back, and delete. Runs in CI on a self-hosted or
  privileged runner; locally via the script.
- UI: vitest + testing-library per screen, as in Vyasa.

## 10. Phases

1. **MVP** (this plan): single server, native runtime, Vyasa + hello
   app, sites/servers/apps/plans/jobs screens, backups local, updates
   with rollback, install script, e2e. Target 0.1.0.
2. Containers (Quadlet runtime), S3 backups, disk quotas, deletion
   grace, 2FA enforcement, site move between servers.
3. Customers and billing (Stripe), plan self-service, multi-server
   placement policies, usage metering, email.
4. Catalogue: public registry of manifests, signed; "Deploy to Setu"
   button spec for app authors.

## 11. Tech stack

Same as Vyasa, deliberately, so one person can hold both in their
head and tooling, CI and skills transfer:

| Concern | Choice |
|---|---|
| Language | Rust stable ≥ 1.85, edition 2021, `forbid(unsafe_code)` |
| Control plane HTTP | axum + tower, utoipa for OpenAPI |
| Database | PostgreSQL 16+, sqlx 0.9 with embedded migrations |
| Jobs | Postgres queue, `FOR UPDATE SKIP LOCKED`, tokio |
| Auth | argon2id, tower-sessions, TOTP (totp-rs) |
| Sealing | chacha20poly1305 (`v1:` scheme as Vyasa) |
| Agent | Rust, tokio, `zbus` for systemd D-Bus (fallback `systemctl`), `tokio-postgres` for the local cluster's admin connection, `reqwest` for Caddy's admin API and downloads |
| UI | Vite + React 19 + TypeScript, TanStack Router/Query/Table, Tailwind, shadcn/ui, vitest; embedded with rust-embed |
| CLI | clap |
| Config | config crate + env |
| Observability | tracing, journald |
| Release | musl static binaries x86_64 + aarch64, GitHub Actions, checksums |
| Repo tooling | pnpm, cargo deny, same lint and format config as Vyasa, superpowers skills, repo agents in `.claude/agents/` |

What is not shared with Vyasa: no GraphQL, no WASM, no AI, no search,
no themes. Setu's API surface is small and REST is enough.

## 12. Open questions for the owner

- Name. "Setu" is a placeholder.
- Repository home: a new org or `vyasa-cms/setu`? Recommendation: a
  new org, since Vyasa is one app among many.
- Licence: AGPL-3.0 (keeps hosted forks honest) or Apache-2.0 (more
  adoption). Recommendation: AGPL for the panel, Apache for the agent
  protocol and manifest crates so app authors can embed them.
- Whether phase 1 should already include the Cloudflare tunnel as an
  alternative to Caddy for operators without a public IP (the owner's
  own setup). Recommendation: phase 2.
