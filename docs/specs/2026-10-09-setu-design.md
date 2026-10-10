# Setu: a control panel for single-binary web apps

Date: 2026-10-09 · **Revision 4 (as built, 2026-10-10)**: revision 3
amended to match what shipped on branch `mvp`. Every passage that
changed cites the ruling that changed it, e.g. "(R118)"; rulings are in
`the ledger (private planning repository)`. Revision 3 followed two
independent review passes; their findings and resolutions, and the
revision 3 → as-built record, are in
`docs/specs/2026-10-09-setu-review.md` · Status: approved in
conversation (owner, 2026-10-09) · Working name: **Setu** (bridge).
Rename freely; the name appears only in the binary, crate names, unit
names and the UI title.

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
  on a unix socket `/run/setu/http.sock` (0660, `setu:caddy`, in a
  0755 runtime directory, R112/R126) when Caddy is local (the
  installer's default), or on a TCP address; Caddy in front terminates
  TLS for the panel's hostname.
- **`setu agent run`** — one per managed server, as user `setu` under
  `setu-agent.service`. Supervises sites as systemd units through a
  small privileged helper, manages the local Postgres, programs Caddy
  through a unix-socket admin API, streams logs and metrics. Keeps
  `/var/lib/setu/state.json` and reconciles against Caddy on start and
  every 5 minutes (R98). Talks outbound only, over HTTPS (`http://` is
  accepted for loopback addresses and `localhost`, anything else only
  with `--insecure`, used by tests; `--insecure` never disables
  certificate checks, R99), with a per-server token. The control plane
  never SSHes.
- **`setu helper <verb>`** — the privileged helper, invoked by the
  agent through `sudo -n` with a fixed path. Typed verbs with fixed
  templates and strict input validation inside the helper itself,
  never raw file writes, never a path from the caller (§4).
- `setu migrate`, `setu operator create --email E [--role R]` (password
  from `SETU_OPERATOR_PASSWORD` or the first stdin line, R113), `setu
  backup --out <file>` (its own database, R113), `setu setting set
  <key> <value>` (only `tls_internal` `true|false`, R124), `setu
  openapi [--out]` (writes the OpenAPI document).

Why one binary: the roles share most dependencies, the installer
downloads one archive, every server runs one version. Why native
systemd and not Docker: half the memory per site, instant start,
cgroups give limits and accounting, journald gives logs, and it
matches how Vyasa already installs. Why an agent and not SSH: no root
keys on the control plane, a typed API, reconciliation when the control
plane is down.

Runtime abstraction, fixed now so phase 2 does not rewrite the agent.
As built it is keyed by the site's 8-character short id, and only the
calls that need the spec's content take a `SiteSpec` (R85):

```rust
pub type SiteId = String; // the 8-char short id

#[async_trait]
pub trait Runtime {
    async fn install(&self, site: &SiteSpec, binary: &BinaryRef) -> Result<()>; // fetch + verify into bin/<version>; never touches `current`
    async fn write_unit(&self, site: &SiteSpec) -> Result<()>;  // per-instance drop-in
    async fn start(&self, id: SiteId) -> Result<()>;
    async fn stop(&self, id: SiteId) -> Result<()>;
    async fn status(&self, id: SiteId) -> Result<RuntimeStatus>;
    async fn logs(&self, id: SiteId, since: Option<Cursor>) -> Result<LogStream>;
    async fn remove(&self, id: SiteId) -> Result<()>;
}
```

`switch_version` (atomic `current` symlink), `restart`, `health` and
the backup/restore of data dirs are `NativeRuntime` methods outside the
trait in phase 1 (R85); phase 2 decides which of them the trait needs
for `QuadletRuntime`.

Phase 1 ships `NativeRuntime`. Phase 2 ships `QuadletRuntime` (rootless
Podman via systemd Quadlet) behind the same trait; both produce a
systemd unit, so limits, logs and status code are shared.

## 2. App manifest

A TOML file per app version, published at a URL or pasted into the
catalogue. It is the only app-specific knowledge Setu has. Field-level
reference: `docs/MANIFEST.md`.

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
sha256 = "84b5a99beccace445037191ff2bb168e8b0287a3f52d460191a9ce745220debc"
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

The sha256 is the real value of the v0.2.0 release asset (R57); the
manifest is in the validator corpus verbatim
(`crates/manifest/tests/corpus/vyasa_0_2_0.toml`) and the e2e
provisions it.

Env rules: `generate` (`hex32` | `password20`), `from` (`bind_addr` |
`port` | `postgres_url` | `dir` | `run_dir` | `owner_email` |
`site_url`), `value`, or `prompt` (`label`, asked at site creation);
modifiers `secret` and `once`. `postgres_url` is always secret,
whatever the modifier says (R54). `once` requires `secret`; its value
is written to the env file for the first boot only and removed after
the first successful health check (so the app contract is: health
passes only after the once-values have been consumed), while the
control plane keeps it sealed and lets an operator reveal it, each time
with password re-authentication and an audit row (§7; still revealable
after an update, R55). Rollback and env edits never re-emit `once`
values. `generate = "password20"` is 20 characters from `[A-Za-z0-9]`
and `hex32` is 64 hex characters, so no quoting is ever needed in env
files or URLs. A `from = "dir"` must name a dir in `needs.dirs`. `exec`
is a relative path inside the archive matching
`^[A-Za-z0-9._-]+(/[A-Za-z0-9._-]+)*$` with no `..`; `args` (in `[run]`,
R52) contain no control characters; `dirs` match `^[a-z][a-z0-9-]{0,31}$`;
`version` matches `^[0-9A-Za-z.+-]{1,64}$`; `startup_grace_s` is 1–3600
(default 90) and is the `Health` timeout in provision, update and
restore (R53); `value`s and prompt answers contain no control
characters (R54). The helper re-validates version, exec, args and dirs
(§4): the manifest validator protects operators, the helper protects
root. Unknown keys are errors with a line number. Validation is a pure
function with a test corpus of 17 cases (R57). `strategy = "command"`
is refused as "not supported in this version" (R59); `kind = "image"`
validates but provisioning refuses it.

Apps bind to `bind_addr` (`127.0.0.1:<port>`) when they support it;
for apps that only take a port, `from = "port"` exists and the server's
firewall drops inbound traffic to 20000–29999 (§4), so the app is still
reachable only through Caddy.

## 3. Data model (control plane Postgres)

One migration, `0001_init.sql` (rewritten in place before the first
release, R45/R75); enums are `CHECK` constraints.

- `operators`: id, email (unique, lowercase), password_hash (argon2id),
  role (`owner` | `operator` | `viewer`), created_at.
- `servers`: id, short_id (unique), name, hostname, public_ip,
  token_hash, join_token_hash, join_expires_at, join_nonce,
  join_token_sealed (the issued server token, sealed, kept 60 s from
  `joined_at` for an idempotent retry, R76), joined_at,
  is_control_plane (set by `POST /setup/local-server`), agent_version,
  boot_id, arch, os, cpu_cores, memory_mb, disk_mb, last_seen_at,
  cordoned (bool), and the latest heartbeat's `load1`, `mem_free_mb`,
  `disk_free_mb` (nullable, R108), created_at. `status` is computed on
  read: `pending` (not joined), `online` (seen within 60 s),
  `offline`.
- `apps` (id, name, homepage, created_at, updated_at) and
  `app_versions` (id, app_id, version, channel, manifest jsonb
  validated, published_at). Unique `(app_id, version)`.
- `plans`: id, name, memory_mb, cpu_percent, disk_mb, allowed_app_ids
  (`TEXT[]`, null = all), created_at, updated_at. Seeded "Default"
  (512 MB, 100 %, 5 GB).
- `sites`: id (ULID), short_id (8 chars, unique per server, used in
  user, unit, database and path names), name, app_id, app_version,
  server_id, plan_id, domain (unique, normalised: lowercase, IDNA
  ASCII, no wildcard, not an IP), status (`provisioning` | `running`
  | `stopped` | `failed` | `updating` | `deleting`), failed_step,
  port (unique per server, 20000–29999), db_name, db_role (both
  `site_<short>`), env (jsonb, values sealed, generated exactly once at
  creation, R92), owner_email, created_at, updated_at. There is no
  password column: the role password lives inside the sealed
  `postgres_url` value (R92).
- `jobs`: id, kind (`provision_site` | `update_site` | `backup_site`
  | `restore_site` | `delete_site` | `delete_backup` | `env_update` |
  `restart_site` | `stop_site` | `start_site`), site_id, server_id,
  status (`queued` | `running` | `done` | `failed`), payload jsonb
  (`NOT NULL DEFAULT '{}'`, includes the `step` cursor), attempts,
  locked_by, locked_until, log text, error, created_at, started_at,
  finished_at. One active job per site (partial unique index on
  `site_id WHERE status IN ('queued','running')`). `delete_backup` jobs
  (`payload.backup_id`) run on the backup's site while it exists, so
  they serialise with the update or restore that may read the backup,
  and are server-level (`site_id` NULL) once the site is gone (R89,
  R136).
- `agent_commands`: id, server_id, site_id?, command jsonb (secret
  fields nulled by the SQL function `setu_redact_command` whenever the
  row leaves `queued`/`delivered`: on completion, cancel, expiry and
  prune, R75), attempts (diagnostic only, R85), created_at, expires_at,
  delivered_at, completed_at, result jsonb, status (`queued` |
  `delivered` | `done` | `failed` | `cancelled` | `expired`). Pruned
  24 h after `created_at` for any terminal status; non-terminal rows
  older than 24 h are expired and redacted first (R83). Index on
  `(server_id, status, created_at)`.
- `backups`: id, site_id (nullable, `ON DELETE SET NULL`), server_id,
  site_short_id, site_name, domain, kind (`scheduled` | `manual` |
  `pre_update` | `pre_restore` | `pre_delete`), taken_at, size_bytes,
  status (`pending` | `completed` | `failed` | `deleted`, R75). Files
  live on the server at
  `/var/lib/setu/backups/<short>/<backup_id>/{db.dump,dirs.tar.zst}`;
  `delete_backup` jobs remove them (scheduler: scheduled copies beyond
  7, `pre_*` copies after 30 days).
- `audit_log`: operator_id, action, target_kind, target_id, ip, at.
- `site_metrics`: site_id, at, cpu_seconds, memory_bytes, disk_bytes;
  1-minute resolution, pruned after 30 days by the scheduler.
- `sessions`: id_hash (SHA-256 of the 32 random cookie bytes), operator_id
  (`ON DELETE CASCADE`; all rows deleted on password change),
  created_at, expires_at (7 days, sliding). `settings` (key/value
  jsonb): `setup_token_hash` (R105), `tls_internal` (R106/R124).

Migrations are sqlx, forward-only from the first release, applied on
boot under an advisory lock (`0x5e7a`), exactly as Vyasa.

## 4. Agent: privilege model, units, Caddy, Postgres

### Users and files

- `setu` (system user, home `/var/lib/setu`, no login shell): runs
  `setu-agent.service` and, on a control-plane box, `setu.service`.
  Member of `systemd-journal` (reads site logs without sudo) and of the
  `caddy` group (admin socket); `install-system` adds both, `install.sh`
  adds `caddy` (R73/R127).
- `setu-site-<short>`: one system user per site, uid from the 30000–
  39999 range, no shell, home `/srv/setu/sites/<short>`.
- `/srv/setu` and `/srv/setu/sites` are `root:root 0755`, created by
  `agent install` (the helper's no-follow walk assumes root-owned
  parents, R68). `/srv/setu/sites/<short>/` is root-owned 0755 and
  contains: `bin/<version>/` (root 0755, binaries 0755), `bin/current`
  (symlink, root), `data/` (site user, 0750), `run/` (site user, 0750),
  `env` (root:root 0600; PID 1 reads `EnvironmentFile=` as root, the
  site process never needs it).
- Because the site user owns only `data/` and `run/`, it cannot
  replace `env`, `bin` or anything a privileged path later writes.
- `/var/lib/setu` (setu 0750) holds `state.json` (0600),
  `downloads/` (setu 0750) and `backups/` (`setu:setu 0750`, leaf
  directories 0750: `pg_dump` writes to stdout and the agent writes the
  file, so `postgres` never writes in the agent's tree, R80).
- `/etc/setu` is `root:root 0755` (R74): it holds `nftables.conf`
  (root 0644, loaded by root), `agent.toml` (`setu` 0600), `ca.pem`
  (0644, when `--ca` was given) and, on a control-plane box,
  `setu.toml` (`setu` 0600). The agent cannot rewrite anything root
  later loads.

### The helper

`/opt/setu/setu` is root:root 0755 (not writable by `setu`). The agent
runs privileged work only as `sudo -n /opt/setu/setu helper <verb>
--site <short>` with structured input on **stdin** and output on
stdout; nothing path-like, no mode and no owner ever crosses this
boundary. Framing (R61): stdin is exactly one line of JSON (at most
1 MiB), followed by the raw `tar.zst` stream for `restore` only;
stdout is one line of JSON, or the raw `tar.zst` stream for `backup`;
an error is one line on stderr and exit status 1. `--root <dir>` exists
only in builds with the cargo feature `test-root` (never in release
builds; the release workflow checks the binary offers no `--root`,
R129) and is refused when `SUDO_UID` is set, so it cannot be reached
through sudo; under it the helper uses fake users, a fake systemctl
and recording ownership (R67). Verbs:

| verb | input (JSON) | effect |
|---|---|---|
| `site-create` | `{ dirs: [name] }` | useradd `setu-site-<short>` (idempotent, uid range 30000–39999), create the tree incl. `data/<dir>` for each validated dir; returns `{user, uid, gid}` |
| `write-unit` | `{ short_id, version, exec, args, workdir, memory_mb, cpu_percent }` (built by the agent from `SiteSpec`, R63/R85) | validate `exec`, `args`, `version`, `workdir`, non-zero limits; render `setu-site@<short>.service.d/10-setu.conf` from the fixed template with systemd escaping (`%`→`%%`, `$`→`$$`, `\` and `"` escaped, quoted when not plain); temp file + rename; `daemon-reload` if changed; returns `{changed}` |
| `write-env` | `[{name, value}]` | names `^[A-Z][A-Z0-9_]*$` (unique), values without control characters (R54); written in systemd `EnvironmentFile=` syntax, `NAME="value"` with `\`, `"`, `` ` `` and `$` escaped (R64); `env` 0600 root:root via temp file + rename with `O_NOFOLLOW`; errors never quote a value; returns `{vars, changed}` |
| `install` | `{ version, sha256 }` | opens `/var/lib/setu/downloads/<sha256>.tar.gz` with `O_NOFOLLOW` (must be a regular file, must hash to `sha256`), extracts into a root-owned staging dir rejecting `..`, absolute names, symlinks, hard links, device files and any setuid/setgid/sticky bit (the whole install is **refused**, nothing is left behind), then renames staging to `bin/<version>/`; an existing `bin/<version>/` directory is a no-op |
| `switch-version` | `{ version }` | atomic symlink swap of `bin/current` to an existing `bin/<version>` |
| `unit` | `{ op: start\|stop\|restart\|show }` | `start` = `systemctl enable --now`, `stop` = `systemctl disable --now`, `restart` = `enable` then `restart` of `setu-site@<short>.service`, so a site's running or stopped state survives a reboot (R118); `show` returns `ActiveState, SubState, MainPID, ControlGroup, MemoryCurrent, CPUUsageNSec, NRestarts` plus `data_bytes` (`du -sb data/` as root) |
| `backup` | `{ dirs: [name] }` | streams a `tar.zst` (tar + zstd crates, R31) of `data/<dir>`… to **stdout** as root, walking `data/` through file descriptors without following links (symlinks and special files are skipped; a missing dir is skipped); the agent writes the stream into `/var/lib/setu/backups/<short>/<backup_id>/dirs.tar.zst` |
| `restore` | `{ dirs: [name] }` + `tar.zst` on **stdin** | requires the unit inactive; extracts into a root-owned staging dir with the same entry rules as `install`, accepting only the requested dirs, `chown -R` to the site user (dirs 0750, files 0640 or 0750), then per dir renames the old `data/<dir>` into staging and the new one into `data/` (two `renameat`s, not an exchange: `renameat2` is glibc-only in nix and the release is musl, R60), then removes staging |
| `site-remove` | `{}` | `disable --now` the unit, remove the drop-in directory, `daemon-reload`, `userdel`, `rm -rf` the site tree; idempotent |

Not a helper verb: `install-system` (writes the template unit, the
agent unit, sudoers, Caddy config and firewall unit) runs only as real
root from `setu agent install`, `setu agent install-system` and
`install.sh`, which themselves run as root; it is not under `helper`,
so sudo never reaches it.

Every verb validates `--site` against `^[a-z0-9]{8}$`, derives every
path from it, refuses to follow symlinks on each component under
`/srv/setu/sites` (`openat` with `O_NOFOLLOW|O_DIRECTORY` step by step,
not `lstat` then `open`; nix 0.31 so the walk needs no `unsafe`, R60),
writes with temp-file-plus-rename inside root-owned parents, and never
sets special mode bits. Ownership and user changes go through
`Ownership` and `Users` traits so the helper's unit tests run
unprivileged with recording fakes; the real modes and owners are
asserted in the e2e with `stat`. The helper's tests cover: bad site
id, bad version, bad exec/args (newline, `%`, `$`, `..`), bad dir
names, symlinked final component, symlinked parent, hostile archives
and hostile restore tars, `--root` under sudo, idempotent re-runs,
start/stop persisting across a reboot.

`/etc/sudoers.d/setu` (0440) is exactly these two lines (golden file
`crates/agent/assets/sudoers`; `scripts/check-sudoers.sh` compares it
and runs `visudo -cf`, which both classic sudo and sudo-rs accept;
`install-system` runs `visudo -cf` on a temp copy before the rename).
The revision 3 `Defaults:setu !requiretty, env_reset` line is gone:
both are sudo's defaults, and sudo-rs, the default from Ubuntu 25.10,
rejects `requiretty` (R70):

```
setu ALL=(root) NOPASSWD: /opt/setu/setu helper *
setu ALL=(postgres) NOPASSWD: /usr/bin/psql, /usr/bin/pg_dump, /usr/bin/pg_restore
```

`helper *` is safe because every verb's only arguments are `--site
<short>` (validated) and the verb name; everything else is typed stdin.
Every sudo call uses `-n`, so a misconfigured host fails instead of
hanging (R68). `psql` as `postgres` takes SQL on stdin, with the
password set by a `\set pw '<alphanumeric>'` line on stdin and used as
`:'pw'` (never on argv, never in `ps` or the sudo log); `pg_dump` writes
to stdout and `pg_restore` reads stdin, both piped by the agent to and
from its own backup directory. Residual trust, stated in
`docs/SECURITY.md`: whoever controls the control plane controls every
agent at root through the helper's verbs. That is the product's trust
model; the helper bounds *what* can be done, not *who*. Hosted apps
share the kernel, the Postgres instance and loopback: a hosted app can
reach another site's port on 127.0.0.1 and can send forged
`X-Forwarded-For` to apps that trust 127.0.0.1 (as Vyasa does);
SECURITY.md says so.

### Units

`/etc/systemd/system/setu-site@.service` (template, written by
`install-system`, golden file):

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

Per-instance drop-in `setu-site@<short>.service.d/10-setu.conf`, from
the fixed template `crates/agent/assets/dropin.conf.tmpl`:

```ini
[Service]
ExecStart=
ExecStart=/srv/setu/sites/{short}/bin/current/{exec} {args}
WorkingDirectory={workdir}
MemoryMax={memory_mb}M
CPUQuota={cpu_percent}%
```

`{workdir}` is `bin/current/<dir of exec>`, `bin/current` or `data`
under the site tree. A plan change is a drop-in rewrite plus restart.
The cgroup path for metrics comes from `systemctl show -p ControlGroup`
(instances live under `system-setu\x2dsite.slice`), never guessed; the
heartbeat prefers systemd's `MemoryCurrent`/`CPUUsageNSec` and reads
the cgroup files only when systemd tracks no value (R101).

`setu-agent.service` runs as `setu` without `NoNewPrivileges` (it
needs sudo) and, as built, **without** `ProtectSystem=` or
`ReadWritePaths=` (R69, superseding R62): sudo children run in the
agent unit's mount namespace, so a read-only `/etc` breaks the helper's
drop-in writes and `useradd`; the `setu` user is kept out of `/etc` by
file modes instead. The unit (golden file):

```ini
[Unit]
Description=Setu server agent
After=network-online.target postgresql.service
Wants=network-online.target

[Service]
Type=simple
User=setu
Group=setu
ExecStart=/opt/setu/setu agent run
Restart=always
RestartSec=2

[Install]
WantedBy=multi-user.target
```

On a control-plane box `install.sh` writes `setu.service` (R126/R127):

```ini
[Unit]
Description=Setu control plane
After=postgresql.service network-online.target
Wants=postgresql.service network-online.target

[Service]
Type=simple
User=setu
Group=setu
WorkingDirectory=/etc/setu
RuntimeDirectory=setu
RuntimeDirectoryMode=0755
ExecStart=/opt/setu/setu serve
Restart=always
RestartSec=2

[Install]
WantedBy=multi-user.target
```

`WorkingDirectory=/etc/setu` because the control plane reads
`setu.toml` from its working directory (R127). `/run/setu` is 0755,
not 0750, because systemd re-owns a `RuntimeDirectory` to `setu:setu`
before every non-`+` command, so Caddy (group `caddy`) could not
traverse a 0750 directory; access control stays on the socket, 0660
`setu:caddy` (R126). `setu serve` narrows the umask to 0117 around the
socket's `bind`, sets 0660 explicitly, hands it to group `caddy` (a
warning, not an error, when that fails), and refuses, never deletes, a
non-socket at the path (R112). The binary is also linked at
`/usr/local/bin/setu`, because the wizard's install command starts with
a bare `setu` (R127).

### Network

`install-system` installs `setu-nftables.service`, a oneshot
(`After=nftables.service`, because Ubuntu's `/etc/nftables.conf` starts
with `flush ruleset`, and `Before=network-pre.target`; enabled and
**restarted** on every run so a re-run reloads the table, R73) that
loads only `table inet setu` (`add table` / `delete table` / define,
never `flush ruleset`, so Docker's and the operator's rules are
untouched) from `/etc/setu/nftables.conf`: chain `input` hook input,
`iifname != "lo" tcp dport 20000-29999 drop`. Apps that bind
`0.0.0.0:<port>` are therefore unreachable except through Caddy. The
provision `validate` step refuses a port the host already listens on:
the agent's `Inventory` reports `listening_ports` from
`/proc/net/tcp{,6}` (R94). `doctor` asserts the table exists, and the
e2e checks it again after a container restart.

### Caddy

Installed from the official Caddy apt repository (Ubuntu's own package
is old). When any step of the repository install fails on x86_64 (the
repository answered `402` for every path on 2026-10-10), the agent
removes `caddy-stable.list`, installs the other packages from the
distribution, downloads Caddy's own release package
`caddy_2.11.7_linux_amd64.deb` over https, checks it against the
SHA-512 pinned in `install.rs` (taken from the release's checksums
file) and installs that file (R116). `setu agent install-system`
installs the `caddy` package the same way when `dpkg-query` does not
report it installed (R123), which is how `install.sh` gets Caddy.

The package ships `caddy.service` (Caddyfile) and `caddy-api.service`
(`caddy run --environ --resume`). `install-system`: writes the
`caddy-api.service` drop-in (golden file):

```ini
[Service]
ExecStart=
ExecStart=/usr/bin/caddy run --environ --config /etc/caddy/setu.json --resume
RuntimeDirectory=caddy
RuntimeDirectoryMode=0750
Group=caddy
```

writes `/etc/caddy/setu.json` (admin on `unix//run/caddy/admin.sock|0660`,
TCP admin **off** — `doctor` asserts 2019 is closed —, `persist` on, one
server `setu` listening on `:443` with automatic HTTPS and the
automatic HTTP→HTTPS redirect, an `internal` issuer policy under
`apps.tls.automation` when `--tls internal`, and the `setu-panel` route
when `--panel-host` is given); `systemctl disable --now caddy.service`;
`daemon-reload`; enables `caddy-api.service`. It then always applies
the base through the socket: `POST /load` of the base merged with the
running config's routes (running routes whose `@id` the base does not
define, in order, then the base's own, so a new panel route replaces
the old one and site routes survive), so TLS-mode or panel-host changes
take effect. When the socket does not answer it first moves a
**foreign** autosave (`/var/lib/caddy/.config/caddy/autosave.json`
whose admin listener is not our socket, i.e. written by the distro
`caddy.service` with TCP 2019 open) to `autosave.json.setu-foreign`,
restarts `caddy-api` and waits up to 15 s (R73).

Routes are objects with `@id = "setu-site-<short>"` (upstream
`127.0.0.1:<port>`) or `setu-panel`; upsert is `GET /id/<id>` then
`PATCH /id/<id>` when it exists, else `POST
/config/apps/http/servers/setu/routes` (never `PUT`, which Caddy
refuses for an existing id); removal is `DELETE /id/<id>`, a missing id
being fine (R72). The client is hyper 1 HTTP/1 over a `UnixStream`
with `Host: 127.0.0.1` (one of the values Caddy accepts on a unix admin
listener) and a 15 s deadline; verified against Caddy 2.11.7 (R72).
`SetRoute`/`RemoveRoute` carry the site's short id and the agent
refuses a route command without it; it never derives an id from the
domain (R71). The agent reconciles from `state.json` on start and
every `reconcile_secs` (300; `SETU_AGENT_RECONCILE_SECS` overrides for
tests): every site with a recorded domain and port is upserted, plus
`setu-panel` from `agent.toml`; `setu-site-*` routes whose site is
unknown or has no recorded domain are removed; routes Setu did not
write are never touched; unknown site trees and `site_*` databases are
logged, never deleted. Route commands and a reconcile pass share one
lock, so a reconcile never resurrects a route removed meanwhile (R98).
This repairs a Caddy whose autosave was lost. The panel route
(`panel_host` → `unix//run/setu/http.sock`, or `--upstream`) is
recorded in `/etc/setu/agent.toml` on a control-plane box.

### Postgres

`postgresql-16` from Ubuntu 24.04. The agent uses peer auth as
`postgres` through the sudo rule above (`sudo -n -u postgres psql -X
-q -v ON_ERROR_STOP=1`). `create_database` is create-or-alter: stdin is
`\set pw '<value>'`, then `CREATE ROLE … LOGIN` if absent, `ALTER ROLE
… PASSWORD :'pw'`, `CREATE DATABASE … OWNER …` if absent, `REVOKE
CONNECT ON DATABASE … FROM PUBLIC` (a test asserts another site's role
cannot connect). `agent install` revokes `CONNECT` on `postgres` and,
when it exists, on the control plane's `setu` database (R99); the
installer does the same for `setu` and grants it back to role `setu`.
Backups: `sudo -n -u postgres pg_dump -Fc <db>` to stdout, written by
the agent atomically into `backups/<short>/<backup_id>/db.dump` (R80).
**Restore replaces the database**, so objects a failed version created
do not survive: psql `DROP DATABASE … WITH (FORCE)`, then
create-role-if-absent / create-database-if-absent / `REVOKE` without a
`\set pw` (the role and its password survive the drop; a missing role
is created `LOGIN` without a password until the next
`create_database`), then `pg_restore --no-owner --role=<site role> -d
<db>` from stdin (R81). Dirs are restored by the helper (fresh staging,
swap), with the unit stopped.

## 5. Agent protocol

Outbound long-poll: `GET /agent/v1/commands?wait=N` with the server
token returns the next queued command for that server (or 204) and
marks it `delivered`. `N` is capped at 30; `wait=0` means one check and
an immediate 204 (R111). The handler subscribes to a single
`LISTEN setu_commands` notifier before checking the queue, then waits
on a notification or `min(remaining, 5 s)` and re-checks; a dropped
listener connection wakes every waiter (R111). Polls are rate-limited
to 600 per minute per server (R24). `POST
/agent/v1/commands/{id}/result` completes it (a repeat post, or a post
for a cancelled or expired row, is an ok no-op; another server's
command is 404, R25/R83). `POST /agent/v1/heartbeat` every 15 s, from
its own task so a long poll never delays it (R101), carries `boot_id`,
`load1`, `mem_free_mb`, `disk_free_mb` (stored on the `servers` row,
R108), unit states and the per-site metrics batch (sites named by short
id; other servers' sites are ignored, R91). `boot_id` is the kernel's
boot id plus a per-process ULID, so an agent restart counts as a new
boot (R91). `POST /agent/v1/join` exchanges a join token plus an
agent-generated random `nonce` (1–128 characters; an empty one is 400,
R91) for a server token (join token single use, 1 h). A retry within
60 s of `joined_at` with the **same nonce** receives the **same**
server token (kept sealed for that window, then nulled); a different
nonce is refused, so a lost response is recoverable but a bystander who
saw the join token cannot take over the server. Every join failure is
the same 401 (R76). The agent draws one nonce per install and retries
once, with the same request, only when no HTTP response arrived (R99).

Command semantics:
- Every command has `expires_at` (per kind: `install` 15 min, `health`
  `startup_grace_s` + 30 s, `backup`/`restore` 60 min, others 2 min).
  The control plane uses the same duration as the command's TTL and as
  its wait; a wait that times out cancels the row (redacted, never
  delivered late) (R87). The agent rejects an expired command with
  `ok=false, error="expired"`, checked again when the command reaches
  the front of its site's queue, so a command that waited out its TTL
  behind a long install is rejected, not run late (R96). The control
  plane never re-delivers a command it has cancelled or expired.
- The agent persists each result to `state.json` **before** posting it
  and dedupes by command id (last 1000), so a command delivered twice
  (agent restarted between executing and posting) executes once and
  the cached result is posted. Read-only results (`status`,
  `fetch_logs`, `inventory`) are persisted too but leave the cache once
  posted (a `fetch_logs` page can hold 5000 lines), so a redelivered
  read-only id is re-executed (R97). A result `POST` answered 404 counts
  as acknowledged; unposted results are re-posted on start and before
  the next poll after a failed post (R97). `state.json` is owned by one
  in-process store and saved under its lock (temp file 0600, fsync,
  rename, R98).
- The loop keeps polling while commands run: each command runs in its
  own task after taking that site's FIFO slot (a chain of one-shot
  signals per short id, enqueued in delivery order, R96), so one site's
  15-minute `install` never blocks another site or a read-only command,
  which takes no slot; database commands carry no site (R86) but take
  the slot of the `site_<short>` database they name (R137).
- A job step that times out marks its command `cancelled`, then issues
  a fresh command; there is one retry mechanism (the job's: exactly one
  retry on `Timeout` or `Expired`, R87), not two.
- When a heartbeat arrives with a new `boot_id`, every `delivered`
  command for that server is requeued immediately.
- Commands carrying secrets (`write_env`, `create_database`) have those
  fields nulled whenever the row leaves `queued`/`delivered`
  (completion, cancel, expiry, prune); results and logs are redacted on
  the agent side: every non-empty `create_database` password and
  `write_env` value, secret or not, is replaced with `***` in `log` and
  `error` before the result is persisted (R96). `FetchLogs` returns
  application logs verbatim: apps must not print their own secrets, and
  SECURITY.md says so. `JoinRequest` (token and nonce), `JoinResponse`,
  `Command` (prints its redacted JSON, R86), `Config` (database URL)
  and the agent config have redacting `Debug`.
- `FetchLogs` takes a journal cursor and returns `{lines, cursor}`
  (R86); the agent reads the journal as `setu` (group
  `systemd-journal`), not through the helper.

Commands (R85): the `site` field is the `SiteSpec` only where the
agent needs its content, and the site's 8-character short id everywhere
else, so a command for a site whose manifest or row is gone can still
be built: `SiteCreate{site: SiteSpec}`, `CreateDatabase{db, role,
password}`, `DropDatabase{db, role}`, `Install{site, binary}`,
`SwitchVersion{site, version}`, `WriteUnit{site: SiteSpec}`,
`WriteEnv{site, env}`, `Start{site}`, `Stop{site}`, `Restart{site}`,
`Status{site}`, `Health{site: SiteSpec}` (port, path and grace from the
spec), `SetRoute{site_short_id, domain, port}`,
`RemoveRoute{site_short_id, domain}` (R71), `Backup{site, db, dirs,
backup_id}`, `Restore{site, db, role, dirs, backup_id}`,
`DeleteBackup{site, backup_id}`, `SiteRemove{site}`, `FetchLogs{site,
cursor, lines}`, `Inventory` → `{units, sites_on_disk, routes,
databases, listening_ports}` (R94). `SiteSpec` is `{site_id, short_id,
version, port, exec, args, workdir, dirs, limits, health,
startup_grace_s}`: no paths, user, env file or binary; the agent
derives all paths from `short_id`. The envelope is `{id, kind,
expires_at, command}`; `attempts` stays a server-side diagnostic and is
not on the wire (R85). `DeleteBackup` removes
`/var/lib/setu/backups/<short>/<backup_id>` itself (the agent's own
tree, no helper verb; a symlink there is unlinked, never followed;
backup ids are one `[A-Za-z0-9._-]{1,64}` component, R91).

Agent install (`setu agent install --control-plane URL [--tls
internal] [--insecure] [--ca <file>] [--panel-host H [--upstream U]]
[--config <path>]`, run as root; the join token is read from
`SETU_JOIN_TOKEN`, else the first line of stdin, never argv (R99);
`http://` to a loopback address or `localhost` is accepted without
`--insecure`): idempotent; in order (R74): `apt` (Caddy's repository,
then `sudo gnupg postgresql-16 caddy nftables zstd`, with the R116
fallback), `systemctl enable --now postgresql`, user `setu`,
directories (§4), the Postgres revoke, `install-system` (Caddy config
and service drop-in, unit template, `setu-agent.service`, sudoers,
firewall unit, group memberships), the join, then `systemctl enable` +
`restart setu-agent.service` (the token exists before the first start,
so the agent never crash-loops). `/etc/setu/agent.toml` is created 0600
from the start (random temp name, `O_CREAT|O_EXCL`, fsync, rename) and
then given to `setu`; it holds the control-plane URL, server id and
token, `insecure`, `ca`, `panel` and the agent's paths and intervals
(R99). `--ca` must parse as PEM; it is copied to
`/etc/setu/ca.pem` (0644) and trusted by every control-plane client.
`setu agent install-system [--caddy-only] [--tls internal]
[--panel-host H --upstream U]` runs the system part alone (R73/R123);
`setu agent join` redeems a join token without reinstalling. `setu
agent doctor` checks every item and exits 1 on any failure; its rows
are `helper` (`/opt/setu/setu` a regular file root:root exactly 0755 in
a root-owned directory without group/other write), `sudoers` (`visudo
-cf`), `site-template`, `caddy-api` (active), `caddy-socket`
(reachable), `caddy-tcp-2019` (closed), `nftables` (table present),
`postgres` (`sudo -n -u postgres psql -X -c 'select 1'`), `state-file`,
`server-token`, `control-plane` (`/healthz`) (R100).

## 6. Flows

Every job is a list of named steps; `JobCtx::step` stores the step name
as the cursor before running it and logs `step <name>: ok|error <msg>`
with every registered secret (stored env secrets, the password inside a
database URL, `env_update` changes, update prompt answers) replaced by
`***` in the line, `jobs.log` and the returned error. A resumed job
skips the steps before the cursor and re-runs the cursor step (every
step is idempotent) (R93).

**Provision** (`provision_site`), steps in order:
1. `validate`: domain unique (excluding this site), plan allows the
   app, manifest `run.kind` is `binary`, server online and not cordoned
   (checked on the job's clock), and the site's port not in the agent's
   `listening_ports` unless the site's own unit is active (a retry
   after `start`; the step sends `Inventory`, R94). Capacity is not
   re-checked here: it was admitted once, at `POST /sites` (R82). Port,
   `short_id`, `db_*` and the env were allocated at `POST /sites` inside
   one transaction that locks the server row (sum of plan memory of the
   server's **other** sites, any status, + this site's plan memory +
   256 MB for Postgres and Caddy + 512 MB when `is_control_plane` ≤ 85 %
   of server memory; a server with unknown memory is refused; auto-pick
   ranks online, uncordoned servers by memory left, R82), so there is
   no race here. The env, including a fresh role password inside the
   `postgres_url` value and the prompt answers, is resolved and sealed
   in that transaction and never regenerated; provision only reads it
   (a rule missing from storage fails `validate`) (R92).
2. `site_create`, 3. `create_database` (create-or-alter, so a retry
   with the same sealed password converges), 4. `install` (at most two
   per server at once), 5. `switch_version`, 6. `write_unit`, 7.
   `write_env`, 8. `start`, 9. `health` (the agent polls
   `127.0.0.1:<port><health>` every 500 ms until 2xx or
   `startup_grace_s`, R101), 10. `set_route`, 11. `verify` (after a
   3 s delay, `status` must be active; otherwise `remove_route` and
   fail; R94), 12. `finish`: `write_env` again without `once` values;
   status `running`.
On any error: status `failed`, `failed_step` set, artefacts kept.
Retry (`POST /sites/{id}/retry`, failed sites only) re-runs from step 1
with the same allocations and env.

**Update** (`update_site` to version V, `POST /sites/{id}/update
{version, prompts?}`), steps: 1. `validate` (V exists for the app and
is `binary`), 2. `env`: `setu_manifest::merge` re-resolves the env
against V's manifest, keeping existing `generate` and `prompt` values
by name, re-resolving `value` and `from` (the stored `postgres_url` is
passed in, since the role password is minted once, R55), adding new
rules (answers for new `prompt` rules come `v1:`-sealed in
`payload.prompts`, are opened, registered for redaction and nulled once
the merged env is stored, R110), dropping removed ones and never
re-emitting `once`; the merged env is stored in `sites.env` (`once`
values stay sealed there for reveal, R55); the old env and the old
`SiteSpec` are kept `v1:`-sealed in the job payload, 3. `backup`
(`pre_update`: database and dirs), 4. `install` V (fails here on a bad
checksum or hostile archive: the running site is untouched), 5.
`stop`, 6. `switch_version` V, 7. `write_unit`, 8. `write_env`, 9.
`start`, 10. `health`, 11. `finish` (sets `app_version`). Failure
rules (R93): before `stop`, the old env is put back and the site stays
`running`; at `stop`, `switch_version`, `write_unit` or `write_env` the
rollback runs **without** a restore (the new version never ran); at
`start` or `health` it restores the `pre_update` backup (database
replaced and dirs swapped: the new version may have migrated). The
rollback is one step (`rollback`, so a resumed job re-runs it): `stop`,
`switch_version` old, `write_unit` old, `write_env` old (without
`once`), [`restore`], `start`, `health`. On success the site ends
`running` on the old version with its old env and the job ends
`failed` with an error starting `rollback: <step> failed (…)`
(ruling: `running` is the truthful state); if any rollback command
fails the site ends `failed` with `failed_step = rollback` and the
error `rollback failed: …`.

**Backup** (`backup_site`, `payload.kind` `manual` | `scheduled`,
R89): `Backup` command (`pg_dump` + dirs `tar.zst`) into
`/var/lib/setu/backups/<short>/<backup_id>/` on the server; the
`backups` row ends `completed` with the agent's `size_bytes` (R95).
The scheduler (one tick per minute) enqueues one nightly backup per
`running` site at 03:00 in the control plane's local time
(`SchedulerConfig.utc_offset`, `None` = the system zone: servers report
no zone, and on the single-box install it is the same host) ± a
per-site jitter of up to 20 minutes (an FNV hash of the site id). A
site is due from its jittered time for two hours (covering a control
plane restart around 03:00) and is skipped when its newest `scheduled`
backup (any status) is newer than due − 12 h or it has an active job
(R89). It also enqueues `delete_backup` jobs for scheduled copies
beyond the newest 7 per site row and `pre_*` copies older than 30 days
(manual copies stay until an operator deletes them; one active
`delete_backup` per backup; a busy site's delete waits for the next
tick, R136), and prunes metrics (30 d), finished agent
commands (24 h) and expired sessions. **Restore** (`restore_site`),
steps: `validate` (a `completed` backup of this site), `backup`
(`pre_restore`), `stop`, `restore` (database replaced, dirs swapped;
`role = site.db_role`, dirs from the manifest's backup list), `start`,
`health`, `finish`. A failing `pre_restore` backup or `stop` leaves the
site `running` (R95).

**Delete** (`delete_site`), steps: `backup` (`pre_delete`, kept 30
days), `remove_route`, `stop`, `drop_database`, `site_remove`, `finish`
(delete the row; backups keep `site_name` and `domain`). A failing
`pre_delete` backup aborts the delete with the site untouched, except
for a `failed` site (which may never have had a database or files),
where the failure is logged and the delete proceeds; status `deleting`
is set after the backup; a resumed delete whose row is already gone
succeeds (R95).

**Start/stop/restart** (`start_site`, `stop_site`, `restart_site`:
the `Start`, `Stop` and `Restart` commands, i.e. `enable --now`,
`disable --now`, `enable` + `restart`, R95/R118) and **env_update**
(`PUT /sites/{id}/env` for `prompt`/`value` variables; `payload.changes`
holds each new value `v1:`-sealed; the job merges, stores, writes the
env without `once`, then restarts a running site, R92) are jobs too, so
they serialise with the rest.

**Job runner**: claims with `FOR UPDATE SKIP LOCKED`, sets `locked_by`
(`runner-<ulid>-<n>`, unique per process, R88) and `locked_until =
now() + 60 s`, renews every 20 s; a renewal that finds the lease gone
stops the job without finishing it (the new holder will); a job whose
lease expired is reclaimable (control plane restart, crash). On boot
the single-instance runner also resets `running` jobs with an expired
lease to `queued`. Concurrency 4, at most one job per site (index), at
most two `install` commands per server at once (a semaphore per server
in the runner, tested, R87).

## 7. REST API and UI

API `/api/v1`, JSON, errors `{ "error": { code, message } }` with codes
`validation` 400, `unauthorized` 401, `forbidden` 403, `not_found` 404,
`conflict` 409, `rate_limited` 429 (R19), `agent_unreachable` 502,
`internal` 500.

Sessions (R102): our own `sessions` table; the cookie `setu_session`
carries 32 random bytes (base64url), only their SHA-256 is stored;
`HttpOnly`, `SameSite=Lax`, `Path=/`, `Secure` exactly when
`public_url` is https; 7 days, slid when the last refresh is over an
hour old and re-sent on every authenticated response; login destroys a
session the browser already carried and issues a fresh one; a password
change deletes every session of that operator (your own change issues
this browser a fresh cookie).

Cross-origin guard (R103): every `POST`/`PUT`/`DELETE`/`PATCH` under
`/api/v1`, **login and setup included**, needs either an `Origin`
equal to `public_url` (scheme, host, port) or `Sec-Fetch-Site:
same-origin`; a request with neither is 403. Scripts therefore send
`Origin: <public_url>`. `/agent/v1` (bearer tokens) is outside the
guard.

Client IP (R104): when the panel listens on its unix socket the peer is
Caddy by construction and `X-Forwarded-For` is believed; on TCP it is
believed only from `trusted_proxies` (default `127.0.0.1/32`). Within a
believed header the client is the rightmost hop not in
`trusted_proxies` (all hops trusted → the leftmost); an unparsable hop
voids the header (unix peer: unknown; TCP: the peer). v4-mapped
addresses are canonicalised. Hosted apps on the same box cannot reach
the unix socket (0660, group `caddy`), so they cannot forge the header
at the panel. Login rate limit: 10 failures per IP per 15 min → 429;
an unknown IP shares one bucket and audits `ip = NULL`; wrong reveal
passwords count in the same limiter. Passwords are argon2id; an unknown
email is verified against a dummy hash so its timing matches. Roles:
`owner` everything; `operator` everything except operators and plans
(and `POST /setup/local-server`, owner-only, R106); `viewer` read only.
Every mutation writes `audit_log` `(operator_id, action, target_kind,
target_id, ip)`.

Setup token (R105): on boot with zero operators `setu serve` mints a
token when no hash is stored **or** the hash is stored but
`<data_dir>/setup-token` is gone (the token is only ever readable from
that file, so a lost file would lock the install out); it stores the
SHA-256 hex in `settings.setup_token_hash` and writes the file 0600
from creation. `POST /setup {token, email, password}` answers 409 when
any operator exists, then 400 on bad input (a missing `token` too),
then 401 on a wrong token; the check, the owner insert and the hash
deletion share one transaction under an advisory lock (R44), and the
file is removed after commit.

Resources (as built; `openapi.json` is the full list):

- `setup`: `GET /setup` → `{needed}`; `POST /setup`; `POST
  /setup/local-server` (owner) → a pending server with
  `is_control_plane`, the join token and the install command. The
  command never embeds the token, which is shown separately for
  `SETU_JOIN_TOKEN` (R106). Its shape (R106/R125): on a TCP listen,
  `setu agent install --control-plane http://127.0.0.1:<port>` (`[::1]`
  for an IPv6 listen); on a unix listen, `setu agent install
  --control-plane <public_url> --panel-host <public host>`, plus
  `--upstream unix/<path>` when the socket is not
  `/run/setu/http.sock`, plus `--tls internal --ca
  /var/lib/caddy/.local/share/caddy/pki/authorities/local/root.crt`
  when `settings.tls_internal` is `true`.
- `auth`: `POST /auth/login`, `POST /auth/logout`, `GET /auth/me`.
- `operators`: list and create (owner), `PUT /operators/{id}/password
  {current_password?, password}` (your own needs the current password,
  403 on mismatch; anyone else's is the owner's, R102).
- `servers`: list, get, `POST` (returns the install command and,
  separately, the join token), `DELETE` (409 while it has sites),
  `POST /servers/{id}/cordon`. `ServerOut` carries `status`,
  `cordoned`, `is_control_plane`, `load1`, `mem_free_mb`,
  `disk_free_mb` (R108).
- `apps`: list, `POST /apps/validate` with raw TOML, `POST /apps` with
  TOML or `{url}` fetched with the public-host guard (every resolved IP
  public, no redirects, 10 s, 1 MiB; residual DNS-flip window accepted,
  R20), `GET /apps/{id}/versions`.
- `plans`: list and get (viewer+), create and `PUT` (owner).
- `sites`: `POST` → 202 `{site, job}`; list; `GET /sites/{id}` →
  `SiteDetailOut` = the site plus `metrics: {at, cpu_seconds,
  memory_bytes, disk_bytes} | null` (lists carry no metrics, R108);
  `restart|stop|start|retry` → 202 job; `POST /sites/{id}/update
  {version, prompts?}` → 202 job (R110); `DELETE /sites/{id}` → 202
  `{job}`; `GET /sites/{id}/env` (masked for every role when the stored
  `secret` flag is set or the manifest sources it from `postgres_url`,
  R84), `PUT /sites/{id}/env` → `env_update` job (R92); `POST
  /sites/{id}/env/{name}/reveal {password}` (operator+) → the
  plaintext; a wrong password is 403 `forbidden` (the UI reads 401 as a
  lost session) and every call past authorisation is audited,
  `env.reveal` when the value is returned and `env.reveal_denied`
  otherwise, target `<site_id>:<NAME>` (R107); `GET /sites/{id}/logs`
  (SSE over the journal cursor); `GET|POST /sites/{id}/backups`, `POST
  /sites/{id}/backups/{bid}/restore`, `DELETE /sites/{id}/backups/{bid}`
  (operator+, `delete_backup` job on the site; 400 for another site's
  backup, 409 when already deleted, a delete is active or the site has
  an active job, R109, R136).
- `backups` (top level, R117): `GET /backups` (every non-deleted
  backup newest first, including those of deleted sites with `site_id:
  null`; `BackupOut` has nullable `site_id`, `site_name`, `domain`,
  R108) and `DELETE /backups/{bid}` (the same job and rules), so a
  `pre_delete` backup can be listed and deleted after its site is gone.
- `jobs`: list, get, `GET /jobs/{id}/log` (SSE).
- `audit`: list.
- `GET /api/v1/openapi.json` (viewer+) is the OpenAPI document (utoipa,
  snapshot-tested, drives the TS client); there is no Swagger UI in
  phase 1 (R114). `/healthz` and `/readyz` are unauthenticated.

UI (TanStack Router/Query/Table, Tailwind, shadcn/ui, Vyasa's admin
conventions): Setup wizard (token → owner → local server with the
install command and the join token shown separately → done), Sites
list and New-site dialog, Site (overview with the latest metrics,
logs, env with reveal behind the operator's password, backups with
delete, updates with prompt answers, limits, danger), Servers (list
with load and the control-plane badge, cordon, add with install
command, detail), Apps (catalogue, add with inline validation errors),
Plans, Jobs (list, live log), Settings (operators, password change,
panel URL, trusted proxies, backup defaults). Status colours: running
green, failed red, provisioning/updating amber, stopped gray.

## 8. Install and operations

`install.sh` (one source in the repo, attached to each release; the
owner decides when the repo and releases become public, which is a
prerequisite for `curl | sh` to work on a fresh box). Flags (R127):
`--panel-host <dns>` (required unless `--tls internal`), `--tls
internal` (Caddy's internal CA; alone, the panel host is the first
`hostname -I` address; with `--panel-host`, an internal-CA certificate
for that name), `--version <v>` (default: the latest release),
`--archive-url <url>` and `--sums-url <url>` (together or not at all;
tests). Behaviour, in order: requires root, Linux x86_64 and Ubuntu
(other Ubuntu releases than 24.04 warn, other distributions are
refused); `apt-get install curl ca-certificates postgresql-16 sudo
gnupg zstd nftables`; downloads the archive and `SHA256SUMS`, refuses
an archive whose line does not match, installs `/opt/setu/setu`
root:root 0755 (only when the bytes changed) and links
`/usr/local/bin/setu`; creates user `setu`, `/var/lib/setu` (setu 0750)
and `/etc/setu` (root 0755); creates the `setu` role and database with
a 48-hex-character password from `/dev/urandom` handed to `psql` by
`\set` in a heredoc on stdin and used through `format(… %L, :'pw')
\gexec`, then `REVOKE CONNECT ON DATABASE setu FROM PUBLIC` and `GRANT
CONNECT … TO setu`; writes `/etc/setu/setu.toml` (setu 0600, created
under `umask 077`) with `listen = "unix:/run/setu/http.sock"`,
`database_url`, a generated 64-hex `secret_key`, `public_url =
"https://<panel host>"`, `trusted_proxies = ["127.0.0.1/32"]` and
`data_dir = "/var/lib/setu"`; runs `setu agent install-system
--caddy-only --panel-host <host> --upstream unix//run/setu/http.sock
[--tls internal]` (installs Caddy when missing, R123); adds `setu` to
group `caddy`; writes `setu.service` (§4); runs `setu migrate` and
`setu setting set tls_internal true|false` as `setu` from `/etc/setu`
(R124); restarts `setu.service` only when the binary or the unit
changed (or it is not active); waits up to 90 s for `/healthz` through
Caddy (`curl -k --resolve <host>:443:127.0.0.1`: it checks Caddy → the
socket, not DNS or ACME); prints the setup URL and the token from
`/var/lib/setu/setup-token`. A re-run keeps `setu.toml` (its database
password and secret key: rotating either is a total outage), fails
loudly when `database_url`, `secret_key` or `public_url` cannot be read
back or `public_url` differs from the flags, and is otherwise a no-op
(R127). The wizard's last step runs `setu agent install` for the local
box through the install command it displays.

Config: `setu.toml` in the working directory and `SETU_*` (prefix
separator `_`, nested `__`, R5): `listen` (`unix:<abs path>` or
`<ip>:<port>`, default `127.0.0.1:7000`; replaces revision 1's
`bind_addr`, R48), `database_url`, `secret_key` (64 hex), `public_url`,
`trusted_proxies` (CIDRs or bare IPs; env form comma-separated; default
`["127.0.0.1/32"]`, R50), `data_dir` (default `/var/lib/setu`, R47).
`setu backup --out <file>` runs `pg_dump -Fc -w -f <file>` with
`PGHOST/PGPORT/PGUSER/PGDATABASE/PGPASSWORD` (and `PGSSLMODE`) from the
configured URL in the child's environment, never argv (R113). `setu
setting set` accepts only `tls_internal` with `true`/`false`; the setup
token hash in particular is refused (R124). `/healthz`, `/readyz`.
Logs via tracing to stderr/journald for every role (R121). Releases
(R129): GitHub Actions on `v*` tags; the tag must equal the workspace
version and have a changelog section; the UI and an x86_64 musl static
binary are built without the test features, checked static, same
version, no `--root`; the GitHub release carries
`setu-<ver>-x86_64-unknown-linux-musl.tar.gz`, `install.sh` and one
`SHA256SUMS` covering both. aarch64 and a container image are phase 2.

## 9. Testing

- Unit: manifest corpus (17 cases); env resolution and merge; sealing
  (`v1:` and `plain:` prefixes, key = SHA-256(`"setu-seal-v1"` ||
  secret key), R51); domain normalisation; helper path validation and
  hostile archives; drop-in and unit rendering (golden); sudoers
  (golden + `visudo -cf`); Caddy route JSON and the admin sequence
  against an in-memory Caddy model (R72); cgroup path from
  `ControlGroup`; job state machines over a `FakeAgent` with a recorded
  transcript and `fail_at(kind, nth)` / `timeout_at`; scheduler and
  join tests on an injected clock.
- API integration (`TestDb` per test on a throwaway Postgres, dropped
  `WITH (FORCE)` because listeners hold connections; the `TestServer`
  lives in the api crate's test support, taking a `Router`, so there is
  no crate cycle; long-poll subscribes before checking the queue and
  re-checks every 5 s as a fallback): every endpoint, the role
  matrix, OpenAPI snapshot, lease recovery (claim, kill the runner,
  new runner finishes), command expiry and cancel, secrets never in
  `jobs.log` or `agent_commands` after completion.
- Agent: `FakeHelper`, `FakePsql`, `FakeCaddy` behind traits; the
  helper's own tests run in-process against a temp root, and the
  `--root` CLI path by re-executing the test binary (R67); no test
  needs sudo.
- **End-to-end** (`scripts/e2e.sh`, TAP output, `SETU_E2E_DIR` for
  artefacts and PIDs, built with the cargo features `test-root` and
  `e2e-hooks` which the release workflow never enables): builds the
  x86_64 musl `setu` and `apps/hello` (a C compiler for musl is needed
  for `ring` and `zstd-sys`; without `musl-gcc` the script uses clang,
  R119); creates a dedicated Docker network `setu-e2e-net` (so the
  control plane is not exposed to other containers on this host);
  starts a throwaway control-plane Postgres container; runs `setu serve`
  bound to the host's address on that network; creates the owner with
  the setup token and logs in (every mutation sends `Origin`); starts
  the image `scripts/e2e/Dockerfile` (`jrei/systemd-ubuntu:24.04`, whose
  entrypoint `e2e-init` remounts cgroup2 inside the container's private
  cgroup namespace before exec'ing systemd, R120) as `setu-e2e` on it
  with `--privileged --cgroupns=private` (no cgroup bind mount, no host
  network); copies `setu` in; `setu agent install --tls internal
  --insecure` with `SETU_JOIN_TOKEN` in the environment; `doctor` exits
  0. Setup steps are TAP comments (`Bail out!` on failure), so only the
  assertions are numbered; each `not ok` writes a diagnostics file
  (R122). `hello` versions: 0.1.0; 0.1.1 adds a migration
  (`notes_v2`); 0.1.2 adds another migration (`notes_v3`) and then
  fails health (`HELLO_FAIL = { value = "1" }`), and 0.1.1 refuses to
  boot when `notes_v3` exists, so a rollback that does not replace the
  database is visibly broken; hello's dirs are `uploads` (backed up)
  and `state` (not backed up). The 14 assertions:
  1. `provision_under_60s`;
  2. `serves_over_tls` (`curl -k --resolve`);
  3. `ownership_and_modes` (`env` root 600, `data` site-user 750, `bin`
     root 755);
  4. `public_bind_refused_by_firewall` (a `0.0.0.0:25000` listener in
     the box is refused from the host; `nft list table inet setu`);
  5. `caddy_tcp_admin_closed + site_user_cannot_open_admin_socket`;
  6. `limits_and_metrics` (`MemoryMax`, `memory_bytes > 0`,
     `disk_bytes > 0` in `GET /sites/{id}`);
  7. `backup_restore` (note, back up, another note, restore → only the
     first note and its `uploads` file remain);
  8. `update_to_0_1_1` (`v0.1.1` served, `notes_v2` present);
  9. `rollback_restores_database` (update to 0.1.2 → job failed with
     `rollback: …`, site `running` on 0.1.1, `notes_v3` absent, notes
     intact);
  10. `agent_exit_mid_provision_recovers` (a second site provisioned
      while the agent is told, through a drop-in with
      `SETU_AGENT_TEST_EXIT_AFTER=write_unit` and `Restart=no` that the
      script removes before restarting the agent, to exit after
      executing `write_unit` and before posting: the cached result is
      posted, exactly one `step write_unit:` line, site `running`);
  11. `delete_site` (unit inactive, drop-in gone, user, database and
      directory gone; the `pre_delete` backup listed with the domain at
      `GET /backups`; `DELETE /backups/{bid}` → files gone);
  12. `vyasa_0_2_0` (from its real manifest: `running` in < 60 s,
      `/readyz` 200 over TLS, `/admin` contains `<div id="root"`, reveal
      `VYASA_ADMIN_PASSWORD`, `POST /api/v1/auth/login {email, password}`
      on Vyasa with `Origin: https://vyasa.test` → 200; R122);
  13. `survives_container_restart` (`docker restart -s SIGRTMIN+3`:
      both remaining sites serve over TLS within 60 s, the nft table is
      back);
  14. `reconcile_repairs_lost_autosave` (remove Caddy's autosave and
      restart `caddy-api` → routes return through reconcile,
      `SETU_AGENT_RECONCILE_SECS=10`).
  Then `E2E PASS`. CI runs it on `ubuntu-24.04`.
- `scripts/test-install.sh`: builds the release binary (musl, no
  features), packages it with `scripts/package-release.sh`, serves the
  assets on a private network `setu-install-net`, runs `install.sh
  --tls internal --panel-host setu.test --archive-url … --sums-url …`
  inside a fresh systemd container (the e2e image, `setu.test` →
  127.0.0.1 in its `/etc/hosts`) and asserts, in TAP (R128):
  1. `install_sh_succeeds` (`setu.toml` setu 0600 with the unix
     listener, `/opt/setu/setu` root 0755);
  2. `healthz_through_caddy` (and no TCP listener);
  3. `setup_without_token_401` (a wrong token);
  4. `setup_with_token_201`;
  5. `login`;
  6. `local_server_joins` (the wizard's command, validated against
     `^setu agent install [A-Za-z0-9 ./:_-]+$`, run with
     `SETU_JOIN_TOKEN`; server `online` within 60 s);
  7. `agent_doctor`;
  8. `self_backup` (`setu backup` as `setu` writes a dump `pg_restore
     -l` lists);
  9. `rerun_is_noop` (a second `install.sh` keeps `setu.toml` and the
     same `setu.service` `MainPID`).
  Then `INSTALL PASS`.
- UI: vitest + testing-library per screen.

## 10. Phases

See `docs/ROADMAP.md`. Phase 1 is this document; the review's scope
cuts are listed under Intent.

## 11. Tech stack

Same as Vyasa, deliberately, so one person can hold both and tooling,
CI and skills transfer. As built (workspace `Cargo.toml`):

| Concern | Choice |
|---|---|
| Language | Rust pinned in `rust-toolchain.toml` (1.99, with the `x86_64-unknown-linux-musl` target), edition 2021, `forbid(unsafe_code)`, Vyasa's clippy lints |
| HTTP | axum 0.8 + tower, utoipa 5 (OpenAPI, no Swagger UI) |
| Database | PostgreSQL 16, sqlx 0.9 with embedded migrations; LISTEN/NOTIFY for the agent long-poll |
| Jobs | Postgres queue with leases, `FOR UPDATE SKIP LOCKED`, tokio scheduler |
| Auth | argon2 0.5 (argon2id); sessions in our own `sessions` table over `db.sessions()` — no `tower-sessions` (its sqlx store requires sqlx 0.8; removed with its adapter, R102) |
| Sealing | chacha20poly1305 0.10, Vyasa's KeyVault scheme (`v1:`/`plain:`, SHA-256 key derivation with a label) |
| Agent | tokio; `reqwest` 0.12 with rustls on the `ring` provider (builds for musl without cmake; needs a C compiler, R119) for the control plane and downloads; `hyper` 1 + `hyper-util` + `http-body-util` over a `tokio::net::UnixStream` for Caddy's admin socket; `nix` 0.31 (`fs`, `dir`, `user`) for the helper's fd walk (R60); `tar` + `zstd` + `flate2` for archives (R31); `sudo -n` to the helper and `psql` |
| UI | Vite + React 19 + TypeScript, TanStack Router/Query/Table, Tailwind, shadcn/ui, vitest; rust-embed with `allow_missing` |
| Config | config 0.15 + env |
| Release | x86_64 musl static binary, GitHub Actions, SHA256SUMS |

## 12. Open questions for the owner

- Name. "Setu" is a placeholder.
- Repository home and visibility: decided 2026-10-10 — `setucp/setu`, public; the
  install script needs public releases.
- Licence: AGPL-3.0 for the panel, Apache-2.0 for the manifest and
  protocol crates (recommendation).
- Whether the Cloudflare tunnel alternative to Caddy lands in phase 1
  (recommendation: phase 2).
