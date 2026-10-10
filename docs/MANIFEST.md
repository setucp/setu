# App manifest

A TOML file per app version, published at a URL or pasted into the
catalogue (**Apps → Add**, or `POST /api/v1/apps` with the TOML or
`{url}`). It is the only app-specific knowledge Setu has (spec §2).
Every table in the examples except `[env]` is required; unknown keys
are a validation error naming the key and line number. The validator is
a pure function, with its corpus at
`crates/manifest/tests/corpus/*.toml` (each with an `.expect` file
holding `ok` or the one expected error line). `POST
/api/v1/apps/validate` runs it on raw TOML without storing anything.

## Fields

| Field | Required | Rule |
|---|---|---|
| `schema` | yes | `1` |
| `[app] id` | yes | `^[a-z][a-z0-9-]{1,31}$` |
| `[app] name` | yes | display name |
| `[app] version` | yes | semver, and `^[0-9A-Za-z.+-]{1,64}$` (it names `bin/<version>/`) |
| `[app] homepage` | yes | URL shown in the catalogue |
| `[app] health` | yes | path starting with `/`; `GET 127.0.0.1:<port><health>` must answer 2xx when ready |
| `[app] admin_path` | yes | link shown after provisioning (`"/"` when the app has none) |
| `[app] startup_grace_s` | no | 1–3600, default 90: how long health may fail after a start; it is the health timeout of provision, update and restore |
| `[run] kind` | yes | `"binary"`; `"image"` validates (with `[run.image] image`) but is refused at provisioning until phase 2 |
| `[run] workdir` | no | `"exec_dir"` (default), `"bin"` or `"data"`, see below |
| `[run] args` | no | list of strings, each non-empty, no control characters; systemd-quoted into `ExecStart=` |
| `[run.binary."<target>"] url` | yes | archive URL (`.tar.gz`), downloaded by the agent over https |
| `[run.binary."<target>"] sha256` | yes | 64 hex characters, checked by the agent and again by the root helper |
| `[run.binary."<target>"] exec` | yes | path of the executable inside the archive, `^[A-Za-z0-9._-]+(/[A-Za-z0-9._-]+)*$`, no `..` |
| `[needs]` | yes (may be empty) | |
| `[needs] postgres` | no | e.g. `">=16"`: one database and role per site |
| `[needs] dirs` | no | persistent dirs under `data/`, each `^[a-z][a-z0-9-]{0,31}$` |
| `[env] <NAME>` | no | `^[A-Z][A-Z0-9_]*$`, one rule each (below) |
| `[limits.default]` | yes | `memory_mb`, `cpu_percent` (100 = one core), `disk_mb` (reported, not enforced); the plan's limits override them |
| `[backup] postgres` | yes | bool: dump the database in every backup |
| `[backup] dirs` | no | subset of `needs.dirs` to archive (derivable dirs can be left out) |
| `[update] strategy` | yes | `"replace"`, the only strategy; `"command"` is refused as not supported in this version |

Targets: `x86_64-linux` is mandatory; `aarch64-linux` is accepted
(Setu ships no aarch64 build until phase 2). Archives may not contain
symlinks, hard links, devices, absolute names, `..` or
setuid/setgid/sticky bits: the helper refuses the whole install.

### `workdir`

The site's binary lives at `/srv/setu/sites/<short>/bin/current/<exec>`
(`current` is a symlink to `bin/<version>/`). `WorkingDirectory=` is:

| `workdir` | directory |
|---|---|
| `exec_dir` (default) | the directory holding `exec`, e.g. `bin/current/vyasa-0.2.0-x86_64-unknown-linux-gnu` for an archive with a top-level folder |
| `bin` | `bin/current` (the archive root) |
| `data` | `data/`, the site user's persistent directory |

Only `data/` and `run/` are writable by the app; `bin/` is root-owned.

## Env rules

Each `[env]` entry is exactly one of four sources:

| Source | Syntax | Value |
|---|---|---|
| generate | `{ generate = "hex32" }` | 64 hex characters |
| | `{ generate = "password20" }` | 20 characters of `[A-Za-z0-9]` (no quoting ever needed) |
| from | `{ from = "bind_addr" }` | `127.0.0.1:<port>` |
| | `{ from = "port" }` | `<port>` (20000–29999); the firewall keeps it off the network even if the app binds `0.0.0.0` |
| | `{ from = "postgres_url" }` | `postgres://site_<short>:<password>@127.0.0.1:5432/site_<short>`; always secret |
| | `{ from = "dir", dir = "<name>" }` | `/srv/setu/sites/<short>/data/<name>`; `<name>` must be in `needs.dirs` |
| | `{ from = "run_dir" }` | `/srv/setu/sites/<short>/run` |
| | `{ from = "owner_email" }` | the site's owner email (New-site dialog) |
| | `{ from = "site_url" }` | `https://<domain>` |
| value | `{ value = "…" }` | the constant; no control characters |
| prompt | `{ prompt = "Label" }` (or `label = "Label"`) | asked in the New-site dialog; no control characters |

Modifiers: `secret = true` masks the value in the UI and API (reveal
needs the operator's password, audited) and seals it `v1:` at rest;
`once = true` (requires `secret`) is described below. Values never
contain control characters, so env files never need more than systemd
quoting.

When values are made: every rule is resolved **once**, at `POST
/sites`, and stored sealed; a provisioning retry reuses them (same
generated keys, same database password). On **update** to a new
version, `generate` and `prompt` values are kept by name, `value` and
`from` are re-resolved (the database URL keeps its password), new rules
are resolved fresh (answers for new prompts are asked by the update
dialog), removed rules are dropped. **Env edits** (`PUT
/sites/{id}/env`) change `prompt` and `value` variables and restart the
site.

### The `once` contract

A `once` value (typically the first admin password) is written to the
env file for the first boot only. After the first successful health
check the env file is rewritten without it, and no update, rollback or
env edit writes it again. The control plane keeps it sealed so an
operator can reveal it (with password re-entry, audited) at any time.
So the app must consume it before it reports healthy: create the admin
account on boot, then let `health` pass.

### Health

`health` is polled on `127.0.0.1:<port>` every 500 ms until it answers
2xx or `startup_grace_s` runs out; a failure fails the provision step,
or triggers the rollback of an update. The app should return 2xx only
when it can serve (database migrated, once-values consumed).

## Example: Vyasa 0.2.0

The reference app, exactly as in the corpus
(`crates/manifest/tests/corpus/vyasa_0_2_0.toml`) and provisioned by
the end-to-end test:

```toml
schema = 1

[app]
id = "vyasa"
name = "Vyasa"
version = "0.2.0"
homepage = "https://vyasa.site"
health = "/readyz"
admin_path = "/admin"
startup_grace_s = 90

[run]
kind = "binary"
workdir = "exec_dir"
args = ["serve"]
[run.binary."x86_64-linux"]
url = "https://github.com/vyasa-cms/vyasa/releases/download/v0.2.0/vyasa-0.2.0-x86_64-unknown-linux-gnu.tar.gz"
sha256 = "84b5a99beccace445037191ff2bb168e8b0287a3f52d460191a9ce745220debc"
exec = "vyasa-0.2.0-x86_64-unknown-linux-gnu/vyasa"

[needs]
postgres = ">=16"
dirs = ["media", "index", "registry"]

[env]
VYASA_BIND_ADDR      = { from = "bind_addr" }
VYASA_DATABASE_URL   = { from = "postgres_url" }
VYASA_SECRET_KEY     = { generate = "hex32", secret = true }
VYASA_MEDIA_DIR      = { from = "dir", dir = "media" }
VYASA_INDEX_DIR      = { from = "dir", dir = "index" }
VYASA_REGISTRY_DIR   = { from = "dir", dir = "registry" }
VYASA_RUN_DIR        = { from = "run_dir" }
VYASA_TRUSTED_PROXIES = { value = "127.0.0.1" }
VYASA_ADMIN_EMAIL    = { from = "owner_email" }
VYASA_ADMIN_PASSWORD = { generate = "password20", secret = true, once = true }

[limits.default]
memory_mb = 512
cpu_percent = 100
disk_mb = 5120

[backup]
postgres = true
dirs = ["media", "registry"]

[update]
strategy = "replace"
```

Vyasa binds `127.0.0.1:<port>` (so only Caddy reaches it), runs from
the archive's top-level folder (where its admin assets are), backs up
`media` and `registry` but not the rebuildable `index`, trusts Caddy on
127.0.0.1 for client IPs (see the loopback caveat in
[SECURITY.md](SECURITY.md#what-hosted-sites-share)), and creates its
admin from the one-time password, which the operator reveals in the
site's Env tab.

## Example: `hello` (the e2e app)

`apps/hello/manifest.tmpl.toml`, rendered by `scripts/e2e.sh` once per
version (`__VERSION__`, `__URL__`, `__SHA256__`; `__EXTRA_ENV__` is
`HELLO_FAIL = { value = "1" }` for 0.1.2, whose health then fails):

```toml
schema = 1

[app]
id = "hello"
name = "Hello"
version = "__VERSION__"
homepage = "https://example.com/hello"
health = "/healthz"
admin_path = "/"
startup_grace_s = 20

[run]
kind = "binary"
workdir = "exec_dir"
args = []

[run.binary."x86_64-linux"]
url = "__URL__"
sha256 = "__SHA256__"
exec = "hello"

[needs]
postgres = ">=16"
dirs = ["uploads", "state"]

[env]
HELLO_BIND = { from = "bind_addr" }
DATABASE_URL = { from = "postgres_url" }
HELLO_UPLOADS_DIR = { from = "dir", dir = "uploads" }
HELLO_STATE_DIR = { from = "dir", dir = "state" }
HELLO_NAME = { value = "e2e" }
HELLO_VERSION = { value = "__VERSION__" }
__EXTRA_ENV__

[limits.default]
memory_mb = 512
cpu_percent = 100
disk_mb = 5120

[backup]
postgres = true
dirs = ["uploads"]

[update]
strategy = "replace"
```

The corpus also holds a smaller `hello.toml` (with a `prompt` and a
`generate` rule) used by the unit tests.

## Corpus

| file | expectation |
|---|---|
| `args_control_char.toml` | `error: run.args entries must be non-empty and contain no control characters line 15` |
| `bad_id.toml` | `error: app.id must match [a-z][a-z0-9-]{1,31} line 4` |
| `bad_semver.toml` | `error: app.version must be valid semver line 6` |
| `bad_sha.toml` | `error: run.binary.x86_64-linux.sha256 must be 64 hex chars line 19` |
| `command_strategy.toml` | `error: update.strategy "command" is not supported in this version line 43` |
| `dir_bad_name.toml` | `error: needs.dirs entry "Media_Files" must match [a-z][a-z0-9-]{0,31} line 24` |
| `env_dir_not_declared.toml` | `error: env.HELLO_NOTES_DIR references dir "media" not in needs.dirs line 29` |
| `exec_newline.toml` | `error: run.binary.x86_64-linux.exec must be a relative path of [A-Za-z0-9._-] segments without ".." line 20` |
| `exec_traversal.toml` | `error: run.binary.x86_64-linux.exec must be a relative path of [A-Za-z0-9._-] segments without ".." line 20` |
| `health_without_slash.toml` | `error: app.health must start with / line 8` |
| `hello.toml` | `ok` |
| `image_kind.toml` | `ok` |
| `missing_x86_target.toml` | `error: run.binary must declare x86_64-linux line 17` |
| `once_without_secret.toml` | `error: env.HELLO_TOKEN once requires secret = true line 31` |
| `unknown_key.toml` | `error: unknown field 'colour' at [app] line 11` |
| `vyasa_0_2_0.toml` | `ok` |
| `workdir_bad.toml` | ``error: unknown variant `home`, expected one of `exec_dir`, `bin`, `data` line 14`` |
