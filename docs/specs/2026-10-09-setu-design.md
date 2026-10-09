# Setu: a control panel for single-binary web apps

Date: 2026-10-09 · Revision 3 (after two independent review passes; the
findings and their resolutions are in `docs/specs/2026-10-09-setu-review.md`)
· Status: approved in conversation (owner, 2026-10-09) · Working name:
**Setu** (bridge). Rename freely; the name appears only in the binary,
crate names, unit names and the UI title.

## Intent

A hosting control panel for applications that ship as one binary
(Rust, Go, Zig, Bun-compiled, .NET single-file) or, from phase 2, one
container image. It does for them what cPanel does for PHP: pick an
app, get a domain, a database, TLS, backups, updates and resource
limits, from a web UI, in under a minute, on servers you own. Vyasa is
the first app in the catalogue and the reference customer; the owner
will sell Vyasa hosting plans on it.

Two audiences, in order: (1) the owner operating a fleet of Hetzner
boxes selling plans; (2) self-hosters installing Setu on one box. The
design serves (1) first and never does anything that blocks (2).

Success for phase 1 (0.1.0): on a fresh Ubuntu 24.04 x86_64 VPS with a
public IP and a DNS name, `install.sh` installs Setu behind Caddy with
TLS; the setup wizard (gated by a one-time token) creates the first
operator and registers the same box as a server; "New site → Vyasa
0.2.0 → example.com" yields a working Vyasa with TLS, its own Postgres
database and user, resource limits, a nightly backup and a one-time
admin password the operator can reveal, in under 60 seconds; "Update"
moves it to a newer version and rolls back, database included, when
the new version does not come up healthy; the whole thing is tested,
including one end-to-end run against a real systemd in a container.

Not in phase 1, decided: billing and customers (phase 3); containers as
a runtime (phase 2; the manifest and the `Runtime` trait carry it from
day one); site moves and server drain (phase 2); building from source;
managed runtimes (PHP, Node, Python: never; apps needing an interpreter
ship as an image). Cut from phase 1 by the review to ship sooner: the
container image of Setu itself, aarch64 builds, Swagger UI, TOTP,
operator API tokens and the CLI, an audit page in the UI (the table is
kept), metric charts (numbers are shown), fleet rollout batches,
`update.strategy = "command"`, enforced disk quotas (reported only).

## 1. Shape

One binary, `setu`, from one Cargo workspace and one repository, with
roles selected by subcommand (the k3s model):

- **`setu serve`** — the control plane. One process: REST API,
  embedded web UI (rust-embed), job runner with a scheduler, Postgres
  for its own state. Runs as user `setu` under `setu.service`, listens
  on a unix socket `/run/setu/http.sock` (group `caddy`, 0660) when
  Caddy is local (the installer's default), or on a TCP address;
  Caddy in front terminates TLS for the panel's hostname.
- **`setu agent run`** — one per managed server, as user `setu` under
  `setu-agent.service`. Supervises sites as systemd units through a
  small privileged helper, manages the local Postgres, programs Caddy
  through a unix-socket admin API, streams logs and metrics. Keeps
  `/var/lib/setu/state.json` and reconciles against systemd, Postgres
  and Caddy on every start. Talks outbound only, over HTTPS (refuses
  `http://` unless `--insecure`, used by tests), with a per-server
  token. The control plane never SSHes.
- **`setu helper <verb>`** — the privileged helper, invoked by the
  agent through `sudo` with a fixed path. Typed verbs with fixed
  templates and strict input validation inside the helper itself,
  never raw file writes, never a path from the caller (§4).
- `setu migrate`, `setu operator create` (password from stdin or
  `SETU_OPERATOR_PASSWORD`), `setu backup` (its own database).

Why one binary: the roles share most dependencies, the installer
downloads one archive, every server runs one version. Why native
systemd and not Docker: half the memory per site, instant start,
cgroups give limits and accounting, journald gives logs, and it
matches how Vyasa already installs. Why an agent and not SSH: no root
keys on the control plane, a typed API, reconciliation when the control
plane is down.

Runtime abstraction, fixed now so phase 2 does not rewrite the agent:

```rust
#[async_trait]
pub trait Runtime {
    async fn install(&self, site: &SiteSpec) -> Result<()>;       // fetch + verify into bin/<version>; never touches `current`
    async fn switch_version(&self, site: &SiteSpec, version: &str) -> Result<()>; // atomic `current` symlink
    async fn write_unit(&self, site: &SiteSpec) -> Result<bool>;   // per-instance drop-in; Ok(changed)
    async fn start(&self, site: &SiteSpec) -> Result<()>;
    async fn stop(&self, site: &SiteSpec) -> Result<()>;
    async fn status(&self, site: &SiteSpec) -> Result<RuntimeStatus>;
    async fn remove(&self, site: &SiteSpec) -> Result<()>;
}
```

Phase 1 ships `NativeRuntime`. Phase 2 ships `QuadletRuntime` (rootless
Podman via systemd Quadlet) behind the same trait; both produce a
systemd unit, so limits, logs and status code are shared.

## 2. App manifest

A TOML file per app version, published at a URL or pasted into the
catalogue. It is the only app-specific knowledge Setu has.

```toml
schema = 1

[app]
id = "vyasa"                 # [a-z][a-z0-9-]{1,31}
name = "Vyasa"
version = "0.2.0"            # semver
homepage = "https://vyasa.site"
health = "/readyz"           # GET on the bind address → 2xx means ready
admin_path = "/admin"        # link shown after provisioning
startup_grace_s = 90         # how long health may fail after start

[run]
kind = "binary"              # "binary" | "image" (image: phase 2)
workdir = "exec_dir"         # cwd: "exec_dir" (directory holding exec) | "bin" (bin/current) | "data"
args = ["serve"]
[run.binary."x86_64-linux"]
url = "https://github.com/vyasa-cms/vyasa/releases/download/v0.2.0/vyasa-0.2.0-x86_64-unknown-linux-gnu.tar.gz"
sha256 = "<from the release's SHA256SUMS>"
exec = "vyasa-0.2.0-x86_64-unknown-linux-gnu/vyasa"   # path inside the archive
# [run.binary."aarch64-linux"] … (accepted; built artefacts are phase 2)

[needs]
postgres = ">=16"            # one database + role per site
dirs = ["media", "index", "registry"]   # persistent, under data/, backed up unless excluded

[env]                        # one rule per variable
VYASA_BIND_ADDR      = { from = "bind_addr" }        # 127.0.0.1:<port>
VYASA_DATABASE_URL   = { from = "postgres_url" }
VYASA_SECRET_KEY     = { generate = "hex32", secret = true }
VYASA_MEDIA_DIR      = { from = "dir", dir = "media" }
VYASA_INDEX_DIR      = { from = "dir", dir = "index" }
VYASA_REGISTRY_DIR   = { from = "dir", dir = "registry" }
VYASA_RUN_DIR        = { from = "run_dir" }
VYASA_TRUSTED_PROXIES = { value = "127.0.0.1" }
VYASA_ADMIN_EMAIL    = { from = "owner_email" }
VYASA_ADMIN_PASSWORD = { generate = "password20", secret = true, once = true }

[limits.default]             # overridden by the plan
memory_mb = 512
cpu_percent = 100            # 100 = one core
disk_mb = 5120               # reported, not enforced, in phase 1

[backup]
postgres = true
dirs = ["media", "registry"] # "index" is derivable, excluded

[update]
strategy = "replace"         # the only strategy in phase 1
```

Env rules: `generate` (`hex32` | `password20`), `from` (`bind_addr` |
`port` | `postgres_url` | `dir` | `run_dir` | `owner_email` |
`site_url`), `value`, or `prompt` (`label`, asked at site creation);
modifiers `secret` and `once`. `once` requires `secret`; its value is
written to the env file for the first boot only and removed after the
first successful health check (so the app contract is: health passes
only after the once-values have been consumed), while the control
plane keeps it sealed and lets an operator reveal it, each time with
password re-authentication and an audit row (§7). Rollback and env
edits never re-emit `once` values. `generate = "password20"` is 20
characters from `[A-Za-z0-9]` and `hex32` is 64 hex characters, so no
quoting is ever needed in env files or URLs. A
`from = "dir"` must name a dir in `needs.dirs`. `exec` is a relative
path inside the archive matching `^[A-Za-z0-9._-]+(/[A-Za-z0-9._-]+)*$`
with no `..`; `args` contain no control characters; `dirs` match
`^[a-z][a-z0-9-]{0,31}$`; `version` matches `^[0-9A-Za-z.+-]{1,64}$`.
The helper re-validates all four (§4): the manifest validator protects
operators, the helper protects root. Unknown keys are errors with a
line number. Validation is a pure function with a test corpus.

Apps bind to `bind_addr` (`127.0.0.1:<port>`) when they support it;
for apps that only take a port, `from = "port"` exists and the server's
firewall drops inbound traffic to 20000–29999 (§4), so the app is still
reachable only through Caddy.

## 3. Data model (control plane Postgres)

- `operators`: id, email (unique, lowercase), password_hash (argon2id),
  role (`owner` | `operator` | `viewer`), created_at.
- `servers`: id, short_id, name, hostname, public_ip, token_hash,
  join_token_hash, join_expires_at, join_nonce, join_token_sealed
  (the issued server token, sealed, kept 60 s for an idempotent
  retry), joined_at, is_control_plane (set by `POST /setup/local-server`), agent_version,
  boot_id, arch, os, cpu_cores, memory_mb, disk_mb, last_seen_at,
  cordoned (bool). `status` is computed on read: `pending` (not
  joined), `online` (seen within 60 s), `offline`.
- `apps` (id, name, homepage) and `app_versions` (app_id, version,
  manifest jsonb validated, published_at). Unique `(app_id, version)`.
- `plans`: id, name, memory_mb, cpu_percent, disk_mb, allowed_app_ids
  (null = all). Seeded "Default" (512 MB, 100 %, 5 GB).
- `sites`: id (ULID), short_id (8 chars, unique per server, used in
  user, unit, database and path names), name, app_id, app_version,
  server_id, plan_id, domain (unique, normalised: lowercase, IDNA
  ASCII, no wildcard, not an IP), status (`provisioning` | `running`
  | `stopped` | `failed` | `updating` | `deleting`), failed_step,
  port (unique per server, 20000–29999), db_name, db_role, env
  (jsonb, values sealed, generated exactly once at creation),
    owner_email, created_at, updated_at.
- `jobs`: id, kind (`provision_site` | `update_site` | `backup_site`
  | `restore_site` | `delete_site` | `delete_backup` | `env_update` |
  `restart_site` | `stop_site` | `start_site`), site_id, server_id, status (`queued` | `running` |
  `done` | `failed`), payload jsonb (includes a `step` cursor),
  attempts, locked_by, locked_until, log text, error, created_at,
  started_at, finished_at. One active job per site (partial unique
  index on `site_id WHERE status IN ('queued','running')`).
- `agent_commands`: id, server_id, site_id?, command jsonb (secret
  fields nulled whenever the row leaves `queued`/`delivered`: on
  completion, cancel, expiry and prune), created_at, expires_at,
  delivered_at, completed_at, result jsonb, status (`queued` |
  `delivered` | `done` | `failed` | `cancelled` | `expired`). Pruned
  24 h after `created_at` for any terminal status. Index on `(server_id, status, created_at)`.
- `backups`: id, site_id (nullable, `ON DELETE SET NULL`), server_id,
  site_short_id, site_name, domain, kind (`scheduled` | `manual` |
  `pre_update` | `pre_restore` | `pre_delete`), taken_at, size_bytes,
  status. Files live on the server at
  `/var/lib/setu/backups/<short>/<backup_id>/{db.dump,dirs.tar.zst}`;
  `delete_backup` jobs remove them (scheduler: scheduled copies beyond
  7, `pre_*` copies after 30 days).
- `audit_log`: operator_id, action, target_kind, target_id, ip, at.
- `site_metrics`: site_id, at, cpu_seconds, memory_bytes, disk_bytes;
  1-minute resolution, pruned after 30 days by the scheduler.
- `sessions`: id_hash (SHA-256 of the 32 random cookie bytes), operator_id
  (`ON DELETE CASCADE`; all rows deleted on password change),
  created_at, expires_at (7 days, sliding). `settings` (key/value:
  setup token hash, trusted proxies, backup defaults).

Migrations are sqlx, forward-only, applied on boot under an advisory
lock, exactly as Vyasa.

## 4. Agent: privilege model, units, Caddy, Postgres

### Users and files

- `setu` (system user): runs `setu-agent.service` and, on a control-
  plane box, `setu.service`. Member of `systemd-journal` (reads site
  logs without sudo) and of the `caddy` group (admin socket).
- `setu-site-<short>`: one system user per site, uid from the 30000–
  39999 range, no shell, home `/srv/setu/sites/<short>`.
- `/srv/setu/sites/<short>/` is root-owned 0755 and contains:
  `bin/<version>/` (root 0755, binaries 0755), `bin/current` (symlink,
  root), `data/` (site user, 0750), `run/` (site user, 0750), `env`
  (root:root 0600; PID 1 reads `EnvironmentFile=` as root, the site
  process never needs it), `limits.conf` is not here (see drop-ins).
- Because the site user owns only `data/` and `run/`, it cannot
  replace `env`, `bin` or anything a privileged path later writes.

### The helper

`/opt/setu/setu` is root:root 0755 (not writable by `setu`). The agent
runs privileged work only as `sudo -n /opt/setu/setu helper <verb>
--site <short>` with structured input on **stdin** (JSON) and output on
stdout; nothing path-like, no mode and no owner ever crosses this
boundary. `--root <dir>` exists only in builds with the cargo feature
`test-root` (never in release builds) and is refused when `SUDO_UID`
is set, so it cannot be reached through sudo. Verbs:

| verb | input (JSON) | effect |
|---|---|---|
| `site-create` | `{ dirs: [name] }` | useradd `setu-site-<short>` (idempotent, uid range 30000–39999), create the tree incl. `data/<dir>` for each validated dir |
| `write-unit` | `SiteSpec` | validate `exec`, `args`, `version`, `workdir`; render `setu-site@<short>.service.d/10-setu.conf` from the fixed template with systemd escaping (`%`→`%%`, `$`→`$$`, quoting); temp file + rename; `daemon-reload` if changed; returns `{changed}` |
| `write-env` | `[{name, value}]` | names `^[A-Z][A-Z0-9_]*$`, values without newlines; writes `env` 0600 root:root via temp file + rename with `O_NOFOLLOW` |
| `install` | `{ version, sha256 }` | opens `/var/lib/setu/downloads/<sha256>.tar.gz` with `O_NOFOLLOW` (must be a regular file, must hash to `sha256`), extracts into a root-owned staging dir rejecting `..`, absolute names, symlinks, hard links, device files and any setuid/setgid/sticky bit (the whole install is **refused**, nothing is left behind), then renames staging to `bin/<version>/` |
| `switch-version` | `{ version }` | atomic symlink swap of `bin/current` to an existing `bin/<version>` |
| `unit` | `{ op: start\|stop\|restart\|show }` | `systemctl <op> setu-site@<short>.service`; `show` returns `ActiveState, SubState, MainPID, ControlGroup, MemoryCurrent, CPUUsageNSec, NRestarts` plus `data_bytes` (`du -sb data/` as root) |
| `backup` | `{ dirs: [name] }` | streams `tar --zstd` of `data/<dir>`… to **stdout** as root; the agent writes the stream into `/var/lib/setu/backups/<short>/<backup_id>/dirs.tar.zst` |
| `restore` | `{ dirs: [name] }` + tar on **stdin** | requires the unit inactive; extracts into a root-owned staging dir with the same entry rules as `install`, `chown -R` to the site user, then renames over `data/<dir>` (old dir removed after the swap) |
| `site-remove` | `{}` | stop, remove the drop-in, `userdel`, `rm -rf` the site tree |

Not a helper verb: `install-system` (writes the template unit, sudoers,
Caddy config and firewall unit) runs only as real root from `setu agent
install` and `install.sh`, which themselves run as root; it is not
under `helper`, so sudo never reaches it.

Every verb validates `--site` against `^[a-z0-9]{8}$`, derives every
path from it, refuses to follow symlinks on each component under
`/srv/setu/sites` (`openat` with `O_NOFOLLOW|O_DIRECTORY` step by step,
not `lstat` then `open`), writes with temp-file-plus-rename inside
root-owned parents, and never sets special mode bits. Ownership
changes go through an `Ownership` trait so the helper's unit tests run
unprivileged with a recording fake; the real modes and owners are
asserted in the e2e with `stat`. The helper's tests cover: bad site
id, bad version, bad exec/args (newline, `%`, `$`, `..`), bad dir
names, symlinked final component, symlinked parent, hostile archives
and hostile restore tars, `--root` under sudo, idempotent re-runs.

`/etc/sudoers.d/setu` is exactly (golden file, `visudo -cf` in tests):

```
Defaults:setu !requiretty, env_reset
setu ALL=(root) NOPASSWD: /opt/setu/setu helper *
setu ALL=(postgres) NOPASSWD: /usr/bin/psql, /usr/bin/pg_dump, /usr/bin/pg_restore
```

`helper *` is safe because every verb's only arguments are `--site
<short>` (validated) and the verb name; everything else is typed stdin.
`psql` as `postgres` takes SQL on stdin, with the password set by a
`\set pw '<alphanumeric>'` line on stdin and used as `:'pw'` (never on
argv, never in `ps` or the sudo log); `pg_dump` writes to stdout and
`pg_restore` reads stdin, both redirected by the agent to its own
backup directory. Residual trust, stated in SECURITY.md: whoever
controls the control plane controls every agent at root through the
helper's verbs. That is the product's trust model; the helper bounds
*what* can be done, not *who*. Hosted apps share the kernel, the
Postgres instance and loopback: a hosted app can reach another site's
port on 127.0.0.1 and can send forged `X-Forwarded-For` to apps that
trust 127.0.0.1 (as Vyasa does); SECURITY.md says so.

### Units

`/etc/systemd/system/setu-site@.service` (template, written once by
`setu agent install`, golden file):

```ini
[Unit]
Description=Setu site %i
After=network-online.target postgresql.service
Wants=network-online.target

[Service]
Type=simple
User=setu-site-%i
Group=setu-site-%i
EnvironmentFile=/srv/setu/sites/%i/env
Restart=on-failure
RestartSec=2
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
NoNewPrivileges=yes
ProtectKernelTunables=yes
ProtectControlGroups=yes
RestrictSUIDSGID=yes
ReadWritePaths=/srv/setu/sites/%i/data /srv/setu/sites/%i/run
UMask=0027

[Install]
WantedBy=multi-user.target
```

Per-instance drop-in `setu-site@<short>.service.d/10-setu.conf` holds
`ExecStart=` (reset) then `ExecStart=/srv/setu/sites/<short>/bin/current/<exec> <args>`,
`WorkingDirectory=` (`bin/current/<dir of exec>`, `bin/current` or
`data`), `MemoryMax=`, `CPUQuota=`. `setu-agent.service` runs as `setu`
without `NoNewPrivileges` (it needs sudo) but with `ProtectSystem=full`
and `ReadWritePaths=/var/lib/setu`. A plan change is a drop-in rewrite plus restart. The
cgroup path for metrics comes from `systemctl show -p ControlGroup`
(instances live under `system-setu\x2dsite.slice`), never guessed.

### Network

`setu agent install` installs `setu-nftables.service`, a oneshot that
loads only `table inet setu` (`add table` / `delete table` / define,
never `flush ruleset`, so Docker's and the operator's rules are
untouched) from `/etc/setu/nftables.conf`: chain `input` hook input,
`iifname != "lo" tcp dport 20000-29999 drop`. Apps that bind
`0.0.0.0:<port>` are therefore unreachable except through Caddy. The
port allocator also checks the port is free on the host at
`validate`. `doctor` asserts the table exists after a reboot.

### Caddy

Installed from the official Caddy apt repository (Ubuntu's own package
is old). The package ships `caddy.service` (Caddyfile) and
`caddy-api.service` (`caddy run --environ --resume`). `setu agent
install`: `systemctl disable --now caddy.service`; writes a drop-in for
`caddy-api.service` with `ExecStart=` (reset), `ExecStart=/usr/bin/caddy
run --environ --config /etc/caddy/setu.json --resume`,
`RuntimeDirectory=caddy`, `RuntimeDirectoryMode=0750`, `Group=caddy`;
writes `/etc/caddy/setu.json` (admin on `unix//run/caddy/admin.sock|0660`,
TCP admin **off** — `doctor` asserts 2019 is closed —, `persist` on, one
server `setu` listening on `:443` with automatic HTTPS and the
automatic HTTP→HTTPS redirect, `tls internal` policy when `--tls
internal`); adds `setu` to the `caddy` group; enables and starts
`caddy-api.service`. Because `--resume` prefers the autosave, a
**re-run** of `install-system` applies the new base through the
socket: `POST /load` with the base config plus the routes currently in
the running config, so TLS-mode or panel-host changes take effect and
site routes survive. Routes are objects with `@id = "setu-site-<short>"`;
adding is `POST /config/apps/http/servers/setu/routes`, replacing is
`PATCH /id/setu-site-<short>` (never `PUT`, which inserts a duplicate).
The agent reconciles the full route set from `state.json` on start and
every 5 minutes (`SETU_AGENT_RECONCILE_SECS` overrides for tests) and
repairs a Caddy whose autosave was lost. The panel route (`panel_host`
→ `unix//run/setu/http.sock`) is recorded in `/etc/setu/agent.toml` on
a control-plane box and reconciled like a site route, with id
`setu-panel`.

### Postgres

`postgresql-16` from Ubuntu 24.04. The agent uses peer auth as
`postgres` through the sudo rule above. `create_database` is
create-or-alter: stdin is `\set pw '<value>'`, then `CREATE ROLE …
LOGIN` if absent, `ALTER ROLE … PASSWORD :'pw'`, `CREATE DATABASE …
OWNER …` if absent, `REVOKE CONNECT ON DATABASE … FROM PUBLIC` (a test
asserts another site's role cannot connect). The control plane's own
database gets the same revoke on a single-node install. Backups:
`pg_dump -Fc <db>` to stdout, redirected by the agent into
`backups/<short>/<backup_id>/db.dump`. **Restore replaces the
database**, so objects a failed version created do not survive: `DROP
DATABASE … WITH (FORCE)`, create-or-alter again, then `pg_restore
--no-owner --role=<site role> -d <db>` from stdin. Dirs are restored
the same way (fresh staging, swap), with the unit stopped.

## 5. Agent protocol

Outbound long-poll: `GET /agent/v1/commands?wait=30` with the server
token returns the next queued command for that server (or 204), marks
it `delivered`; `POST /agent/v1/commands/{id}/result` completes it;
`POST /agent/v1/heartbeat` every 15 s carries `boot_id`, load, free
memory and disk, unit states and the 1-minute metrics batch; `POST
/agent/v1/join` exchanges a join token plus an agent-generated random
`nonce` for a server token (single use, 1 h). A retry within 60 s with
the **same nonce** receives the **same** server token (kept sealed for
that window); a different nonce is refused, so a lost response is
recoverable but a bystander who saw the join token cannot take over
the server.

Command semantics, fixed by the review:
- Every command has `expires_at` (per kind: `install` 15 min, `health`
  `startup_grace_s` + 30 s, `backup`/`restore` 60 min, others 2 min). The agent rejects an expired
  command with `expired`; the control plane never re-delivers a
  command it has cancelled or expired.
- The agent persists each result to `state.json` **before** posting it
  and dedupes by command id (last 1000), so a command delivered twice
  (agent restarted between executing and posting) executes once and
  the cached result is posted. The loop keeps polling while commands
  run: each command runs in its own task after taking that site's FIFO
  slot, so one site's 15-minute `install` never blocks another site or
  a read-only command (`status`, `fetch_logs`, `inventory`, which take
  no slot).
- A job step that times out marks its command `cancelled`, then issues
  a fresh command; there is one retry mechanism (the job's), not two.
- When a heartbeat arrives with a new `boot_id`, every `delivered`
  command for that server is requeued immediately.
- Commands carrying secrets (`write_env`, `create_database`) have those
  fields nulled whenever the row leaves `queued`/`delivered`
  (completion, cancel, expiry, prune); results and logs are redacted on
  the agent side (every secret value known to the agent for that
  command is replaced with `***` in `log` and `error`). `FetchLogs`
  returns application logs verbatim: apps must not print their own
  secrets, and SECURITY.md says so. `JoinRequest`, `JoinResponse`,
  `Config` (database URL) and the agent config have redacting `Debug`.
- `FetchLogs` takes a journal cursor and returns the next cursor;
  `agent_commands` is pruned 24 h after completion.

Commands: `SiteCreate{site}`, `CreateDatabase{db, role, password}`,
`DropDatabase{db, role}`, `Install{site, binary}`,
`SwitchVersion{site, version}`, `WriteUnit{site}`, `WriteEnv{site,
env}`, `Start{site}`, `Stop{site}`, `Restart{site}`, `Status{site}`,
`Health{site}` (path and grace from the spec), `SetRoute{domain,
port}`, `RemoveRoute{domain}`, `Backup{site, db, dirs, backup_id}`,
`Restore{site, db, role, dirs, backup_id}`, `DeleteBackup{site,
backup_id}`, `SiteRemove{site}`, `FetchLogs{site, cursor, lines}`,
`Inventory`. `SiteSpec` carries `short_id`, `version`, `exec`, `args`,
`workdir`, `dirs`, `limits`, `port`, `health`, `startup_grace_s`; the
agent derives all paths from `short_id`.

Agent install (`setu agent install --control-plane URL [--tls
internal] [--insecure] [--ca <file>]`, run as root; the join token is
read from stdin or `SETU_JOIN_TOKEN`; `http://127.0.0.1:…` and
`http://localhost:…` are accepted without `--insecure`):
idempotent; creates `setu`, directories, installs `sudo`, `gnupg`,
`postgresql-16`, `caddy` (official repo), `nftables`, `zstd`; runs
`install-system` (Caddy config and service drop-in, unit template,
sudoers, firewall unit, `setu-agent.service`); joins; writes `/etc/setu/agent.toml` (0600,
`setu`) with the server token. `setu agent doctor` checks every item
and exits 1 on any failure.

## 6. Flows

**Provision** (`provision_site`), steps in order, each idempotent and
logged as `step <name>: ok|error …`; the job stores the current step
so a resumed job continues from it:
1. `validate`: domain unique (excluding this site), plan allows the
   app, manifest `run.kind` is `binary`, server online and not
      cordoned, capacity (sum of plan memory of the server's **other**
   sites + this site's plan memory + 256 MB for Postgres and Caddy +
   512 MB when `is_control_plane` ≤ 85 % of server memory, leaving the
   rest for the OS and page cache), port free on the host). Port, `short_id`, `db_*` and the
   env were allocated at `POST /sites` inside one transaction that
   locks the server row, so there is no race here.
2. `site_create`, 3. `create_database` (create-or-alter, so a retry
   with the same sealed password converges), 4. `install`, 5.
   `switch_version`, 6. `write_unit`, 7. `write_env`, 8. `start`, 9.
   `health` (polls the bind address through the agent until 2xx or
   `startup_grace_s`), 10. `set_route`, 11. `verify` (`status` is
   active 3 s after the route; otherwise `remove_route` and fail), 12.
   `finish`: `write_env` again without `once` values; status
   `running`.
On any error: status `failed`, `failed_step` set, artefacts kept.
Retry re-runs from step 1 with the same allocations.

**Update** (`update_site` to version V): 1. `validate` (V exists for
the app and is `binary`), 2. `env`: re-resolve the env against V's
manifest, keeping existing `generate` and `prompt` values by name,
adding new rules, dropping removed ones, never re-emitting `once`;
store the old env and the old `SiteSpec` sealed in the job payload,
3. `backup` (`pre_update`: database and dirs), 4. `install` V (fails
here on a bad checksum or hostile archive: the running site is
untouched), 5. `stop`, 6. `switch_version` V, 7. `write_unit`, 8.
`write_env`, 9. `start`, 10. `health`. On failure at 9 or 10:
`rollback` = `stop`, `switch_version` old, `write_unit` old,
`write_env` old, `restore` from the `pre_update` backup (database
replaced and dirs swapped: the new version may have migrated),
`start`, `health`; the site ends `running` on the old version and the
job ends `failed` with `rollback: true` (ruling: `running` is the
truthful state).

**Backup** (`backup_site`): `pg_dump` + dirs tar into
`/var/lib/setu/backups/<short>/<backup_id>/` on the server; row in
`backups`. The scheduler enqueues one nightly per site at 03:00 server
time with jitter, and `delete_backup` jobs for scheduled copies beyond
7 and `pre_*` copies older than 30 days (manual copies stay until an
operator deletes them). **Restore**: `pre_restore` backup, stop,
restore database (replace) and dirs (swap), start, health.

**Delete** (`delete_site`): `backup` (`pre_delete`, kept 30 days),
`remove_route`, `stop`, `drop_database`, `site_remove`, delete the row
(backups keep `site_name` and `domain`).

**Start/stop/restart** and **env_update** (operator edited `prompt`/
`value` variables: `write_env` without `once`, then restart) are jobs
too, so they serialise with the rest.

**Job runner**: claims with `FOR UPDATE SKIP LOCKED`, sets `locked_by`
and `locked_until = now() + 60 s`, renews every 20 s; a job whose lease
expired is reclaimable (control plane restart, crash). On boot the
single-instance runner also resets `running` jobs with an expired lease
to `queued`. Steps are resumed from the stored cursor; each step is
idempotent so a repeated step is harmless. Concurrency 4, at most one
job per site (index), at most two `install` steps per server at once
(a semaphore per server in the runner, tested).

## 7. REST API and UI

API `/api/v1`, JSON, session cookie (SameSite=Lax, Secure when the
public URL is https, HttpOnly) with an `Origin`/`Sec-Fetch-Site` check
on mutations; errors `{ "error": { code, message } }`. Client IP: when
the panel listens on its unix socket the peer is Caddy by construction
and `X-Forwarded-For` is trusted; on TCP it is trusted only from
`trusted_proxies`. Hosted apps on the same box cannot reach the unix
socket (group `caddy`, 0660), so they cannot forge the header at the
panel. Login rate limit: 10 failures per IP per 15 min. Roles: `owner`
everything; `operator` everything except operators and plans; `viewer`
read only. Every mutation writes `audit_log`.

Resources: `setup` (`GET` → `{needed}`; `POST` with the setup token,
creates the owner under an advisory lock, 409 afterwards; `POST
/setup/local-server` → pending server with `is_control_plane`, and the
install command: `--control-plane http://127.0.0.1:<port>` when the
panel listens on TCP; on a unix-socket install the local agent joins
through Caddy at the public URL, adding `--ca
/var/lib/caddy/.local/share/caddy/pki/authorities/local/root.crt` when
Caddy runs `tls internal`), `auth`
(login, logout, me), `operators`, `servers` (`POST` returns the install
command; the join token is shown separately for `SETU_JOIN_TOKEN=`;
`DELETE` 409 while it has sites; `cordon`), `backups` (`DELETE` → job),
`apps` (`POST /apps/validate` with raw TOML; `POST /apps` with TOML or
`{url}` fetched with the public-host guard: no loopback/private/link-
local, 1 MiB cap, 10 s, no redirects to private hosts), `plans`,
`sites` (`POST` → 202 `{site, job}`; `GET` (includes the latest metric sample); `restart|stop|start|
update|retry|delete` → 202 job; `env` GET masked, `PUT` for `prompt`
and `value` variables → `write_env` + restart job; `POST
/sites/{id}/env/{name}/reveal` with the operator's password → the
plaintext, each call audited (`env.reveal`); `logs` SSE with the journal cursor; `backups` list,
create, `restore`), `jobs` (list, get, `log` SSE), `audit` (list).
OpenAPI from utoipa, snapshot-tested, drives the TS client. No Swagger
UI in phase 1.

UI (TanStack Router/Query/Table, Tailwind, shadcn/ui, Vyasa's admin
conventions): Setup wizard (token → owner → local server → done),
Sites list and New-site dialog, Site (overview, logs, env with reveal,
backups, updates, limits, danger), Servers (list, add with install
command, detail), Apps (catalogue, add with inline validation errors),
Plans, Jobs (list, live log), Settings (operators, panel URL, trusted
proxies, backup defaults). Status colours: running green, failed red,
provisioning/updating amber, stopped gray.

## 8. Install and operations

`install.sh` (one source in the repo; the owner decides when the repo
and releases become public, which is a prerequisite for `curl | sh` to
work on a fresh box): detects x86_64 Linux, downloads the checked
archive, installs `/opt/setu/setu` root:root 0755, creates user
`setu`, installs `postgresql-16` and Caddy (official repo), creates
the `setu` database and role (password generated, `REVOKE CONNECT …
FROM PUBLIC`), writes `/etc/setu/setu.toml` (0600; the role password is written by
the script into the file, never on a command line) with a generated
`SETU_SECRET_KEY`, the database URL, `listen = "unix:/run/setu/http.sock"`
and `public_url = https://<panel host>` (`--panel-host`, or `--tls
internal` with the IP), runs `setu agent install-system --caddy-only
--panel-host …` (Caddy config with the panel route to the unix
socket), writes and starts `setu.service` (`RuntimeDirectory=setu`,
socket group `caddy`). On first boot with zero operators `setu serve`
generates the one-time setup token itself, stores its hash in
`settings` and writes `/var/lib/setu/setup-token` (0600); the script
prints the setup URL and that token. The wizard's last step runs
`setu agent install` for the local box through the install command it
displays.

Config: `setu.toml` and `SETU_*` (prefix separator `_`, nested `__`):
`listen` (`unix:<path>` or `<ip>:<port>`, default `127.0.0.1:7000`),
`database_url`, `secret_key`, `public_url`, `trusted_proxies`,
`data_dir`. `setu backup` passes the database password to `pg_dump`
via `PGPASSWORD` in the child's environment, not argv. `/healthz`, `/readyz`. Logs via tracing
to journald. Releases: GitHub Actions on tags, x86_64 musl static
binary, `SHA256SUMS`, GitHub release. aarch64 and a container image
are phase 2.

## 9. Testing

- Unit: manifest corpus; env resolution; sealing (`v1:` and `plain:`
  prefixes, key derivation as Vyasa's KeyVault); domain normalisation;
  helper path validation; drop-in and unit rendering (golden); Caddy
  route JSON (golden); cgroup path from `ControlGroup`; job state
  machines over a `FakeAgent` with a recorded transcript and
  `fail_at(kind, nth)` / `timeout_at`.
- API integration (`TestDb` per test on a throwaway Postgres, dropped
  `WITH (FORCE)` because listeners hold connections; the `TestServer`
  lives in the api crate's test support, taking a `Router`, so there is
  no crate cycle; long-poll subscribes before checking the queue and
  re-checks every 5 s as a fallback): every endpoint, the role
  matrix, OpenAPI snapshot, lease recovery (claim, kill the runner,
  new runner finishes), command expiry and cancel, secrets never in
  `jobs.log` or `agent_commands` after completion.
- Agent: `FakeHelper`, `FakePsql`, `FakeCaddy` behind traits; the
  helper's own tests run against a temp root with `--root` (unit
  tests do not need sudo).
- **End-to-end** (`scripts/e2e.sh`, TAP output, `SETU_E2E_DIR` for
  artefacts and PIDs, built with the cargo features `test-root` and
  `e2e-hooks` which the release workflow never enables): builds the
  x86_64 musl `setu` and `apps/hello`; creates a dedicated Docker
  network `setu-e2e-net` (so the control plane is not exposed to other
  containers on this host); starts a throwaway control-plane Postgres
  container; runs `setu serve` bound to the host's address on that
  network; starts `jrei/systemd-ubuntu:24.04` as `setu-e2e` on it with
  `--privileged --cgroupns=private` (no cgroup bind mount, no host
  network); copies `setu` in; `setu agent install --tls internal
  --insecure`; joins; `doctor` exits 0. `hello` versions: 0.1.0; 0.1.1
  adds a migration (`notes_v2`); 0.1.2 adds another migration
  (`notes_v3`) and then fails health (`HELLO_FAIL = { value = "1" }`),
  and 0.1.1 refuses to boot when `notes_v3` exists, so a rollback that
  does not replace the database is visibly broken. Assertions:
  provisioning time < 60 s; TLS via `curl -k --resolve`; `stat` shows
  `env` root 600, `data` site-user 750, `bin` root 755; a listener on
  `0.0.0.0:25000` inside the container is refused from the host and
  `nft list table inet setu` exists (again after reboot); TCP 2019
  closed and the site user cannot open the admin socket; `MemoryMax`
  and `memory_bytes > 0` and `disk_bytes > 0` in the site's metrics;
  write a note, back up, write another, restore → only the first
  remains; update to 0.1.1 → `v0.1.1` served; update to 0.1.2 → job
  failed with `rollback`, site `running` on 0.1.1, `notes_v3` absent,
  notes intact; a second site provisioned while the agent is told
  (`SETU_AGENT_TEST_EXIT_AFTER=write_unit`, set once through a drop-in
  that the script removes before restarting the agent) to exit after
  executing `write_unit` and before posting: after `systemctl restart
  setu-agent` the command is redelivered, the cached result is posted
  (the helper ran once), and the site reaches `running`; delete → unit,
  user, database, directory gone, `pre_delete` backup listed with the
  domain; `DELETE` that backup → files gone; **Vyasa 0.2.0** from its
  real manifest: `running` in < 60 s, `/readyz` 200 over TLS, `/admin`
  HTML contains `<div id="root"`, reveal `VYASA_ADMIN_PASSWORD`, log in
  on Vyasa with it → 200; `docker restart setu-e2e` → both remaining
  sites serve over TLS within 60 s; remove Caddy's autosave and restart
  Caddy → routes return through reconcile (`SETU_AGENT_RECONCILE_SECS=10`).
  CI runs it on `ubuntu-24.04`.
- `scripts/test-install.sh`: `install.sh --tls internal --panel-host
  setu.test` against a locally served archive inside a fresh systemd
  container (with `setu.test` added to the container's `/etc/hosts`),
  asserts `/healthz` through Caddy, `/setup` without the token is 401
  and with it creates the owner, and the wizard's local-server command
  joins the box (server `online`).
- UI: vitest + testing-library per screen.

## 10. Phases

See `docs/ROADMAP.md`. Phase 1 is this document; the review's scope
cuts are listed under Intent.

## 11. Tech stack

Same as Vyasa, deliberately, so one person can hold both and tooling,
CI and skills transfer. Versions are pinned to what Vyasa uses today
(checked at plan time, not assumed): see the plan's workspace
dependency list.

| Concern | Choice |
|---|---|
| Language | Rust stable ≥ 1.85, edition 2021, `forbid(unsafe_code)`, Vyasa's clippy lints |
| HTTP | axum + tower, utoipa (OpenAPI, no Swagger UI) |
| Database | PostgreSQL 16, sqlx 0.9 with embedded migrations |
| Jobs | Postgres queue with leases, `FOR UPDATE SKIP LOCKED`, tokio scheduler |
| Auth | argon2id; sessions in our own `sessions` table (migration 0001; repo in `db`, purge by the scheduler) — `tower-sessions-sqlx-store` requires sqlx 0.8, checked 2026-10-09 |
| Sealing | chacha20poly1305, Vyasa's KeyVault scheme (`v1:`/`plain:`, SHA-256 key derivation with a label) |
| Agent | tokio, `reqwest` (rustls with the `ring` provider, which builds for musl without cmake) for the control plane and downloads, `hyper` 1 + `hyper-util` over a `tokio::net::UnixStream` for Caddy's admin socket, `sudo` to the helper and `psql` |
| UI | Vite + React 19 + TypeScript, TanStack Router/Query/Table, Tailwind, shadcn/ui, vitest; rust-embed with `allow_missing` |
| Config | config crate + env |
| Release | x86_64 musl static binary, GitHub Actions, SHA256SUMS |

## 12. Open questions for the owner

- Name. "Setu" is a placeholder.
- Repository home and visibility: `vyasa-cms/setu` is private; the
  install script needs public releases.
- Licence: AGPL-3.0 for the panel, Apache-2.0 for the manifest and
  protocol crates (recommendation).
- Whether the Cloudflare tunnel alternative to Caddy lands in phase 1
  (recommendation: phase 2).
