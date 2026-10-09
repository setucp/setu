# App manifest

A TOML file per app version, published at a URL or pasted into the
catalogue. It is the only app-specific knowledge Setu has. Unknown
keys are a validation error naming the key and line number; the
validator is pure, with its corpus at
`crates/manifest/tests/corpus/*.toml` (each with an `.expect` file
holding `ok` or the one expected error line).

## Minimal example (`hello`, from the corpus)

```toml
schema = 1

[app]
id = "hello"               # [a-z0-9-], unique in the catalogue
name = "Hello"
version = "0.1.0"          # semver, required
homepage = "https://example.com"
health = "/healthz"        # GET → 2xx means ready (must start with /)
port_env = "PORT"          # the app reads its listen port from here
admin_path = "/"           # link shown after provisioning

[run]
kind = "binary"            # "binary" | "image" (image is parsed but the
                           # control plane rejects it: not available in 0.1.0)

[run.binary]
args = ["serve"]
"x86_64-linux" = { url = "https://example.com/hello-0.1.0-x86_64.tar.gz", sha256 = "<64 hex>", exec = "hello" }
# "aarch64-linux" = { … }  at least one of x86_64-linux is required

[needs]
dirs = []                  # persistent dirs under the site's data dir, backed up

[env]
HELLO_PORT = { from = "port" }
HELLO_NAME = { prompt = "Display name", secret = false }

[limits.default]
memory_mb = 256
cpu_percent = 50           # 100 = one core
disk_mb = 1024

[backup]
postgres = false

[update]
strategy = "replace"
```

## Rules per section

- `schema`: currently `1`.
- `[app]`: `id` must match `[a-z0-9-]+`; `version` must be valid
  semver; `health` must start with `/`; all fields required.
- `[run.binary]`: each target needs `url`, `sha256` (exactly 64 hex
  chars), `exec`; `x86_64-linux` is mandatory. `kind = "image"` takes
  `[run.image] image = "…"` and parses, but provisioning rejects it
  with `validation: image runtime not available in this version`.
- `[needs]`: optional `postgres = ">=16"` (a version requirement; one
  database + role per site) and `dirs` (persistent directories).
- `[env]`: every entry is exactly one of:
  - `generate`: `{ generate = "hex32" | "password20", secret = true }`
    (`once = true` additionally requires `secret = true`; `once`
    values are written on first boot only and deleted from the env
    file after the first successful health check);
  - `from`: `{ from = "postgres_url" | "dir" | "run_dir" | "site_url"
    | "owner_email" | "port", … }` — `from = "dir"` additionally
    needs `dir = "<name>"`, and **that dir must be listed in
    `needs.dirs** (else `env.X references dir "…" not in needs.dirs`);
  - `value`: `{ value = "…" }` (a constant);
  - `prompt`: `{ prompt = "Label", secret = true|false }` (asked at
    site creation; `once` allowed only with `secret = true`).
- `[limits.default]`: `memory_mb`, `cpu_percent`, `disk_mb` (all
  required); the plan's limits override these at provisioning.
- `[backup]`: `postgres` (bool), `dirs` (subset of `needs.dirs` —
  unlisted dirs are an error).
- `[update]`: `strategy = "replace"` (fetch, stop, swap, start,
  health, rollback on failure) or `strategy = "command", command =
  "…"` (the app updates itself; no rollback beyond what it does).

## Full example (Vyasa, from the corpus)

See `crates/manifest/tests/corpus/vyasa_0_2_0.toml`: two binary
targets with real release URLs, `needs.postgres = ">=16"`,
`dirs = ["media", "index"]`, generated + derived + prompt env
(including a `once` admin password), and a `replace` update strategy.

## Corpus error cases

Each rejects with one line (see its `.expect` file):

| file | error |
|---|---|
| `unknown_key.toml` | unknown field `'colour'` at `[app]` |
| `bad_id.toml` | `app.id` must match `[a-z0-9-]` |
| `env_dir_not_declared.toml` | `env.VYASA_MEDIA_DIR` references dir `"media"` not in `needs.dirs` |
| `missing_target.toml` | `run.binary` must declare `x86_64-linux` |
| `bad_sha.toml` | `sha256` must be 64 hex chars |
| `health_without_slash.toml` | health path must start with `/` |
| `semver_bad.toml` | version must be valid semver |
| `once_without_secret.toml` | `once` requires `secret = true` |
| `backup_dirs_not_declared.toml` | backup dir not in `needs.dirs` |
