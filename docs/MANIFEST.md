# App manifest

A TOML file per app version, published at a URL or pasted into the
catalogue. It is the only app-specific knowledge Setu has. Unknown
keys are a validation error naming the key and line number; the
validator is pure, with its corpus at
`crates/manifest/tests/corpus/*.toml` (each with an `.expect` file
holding `ok` or the one expected error line).

## Example (`hello`, from the corpus)

```toml
schema = 1

[app]
id = "hello"
name = "Hello"
version = "0.1.0"
homepage = "https://example.com"
health = "/healthz"
admin_path = "/"
startup_grace_s = 30

[run]
kind = "binary"
workdir = "exec_dir"
args = ["serve"]

[run.binary."x86_64-linux"]
url = "https://example.com/hello-0.1.0-x86_64-unknown-linux-musl.tar.gz"
sha256 = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
exec = "hello-0.1.0/hello"

[needs]
postgres = ">=16"
dirs = ["notes"]

[env]
HELLO_BIND = { from = "bind_addr" }
DATABASE_URL = { from = "postgres_url" }
HELLO_NOTES_DIR = { from = "dir", dir = "notes" }
HELLO_NAME = { prompt = "Display name" }
HELLO_TOKEN = { generate = "hex32", secret = true }

[limits.default]
memory_mb = 256
cpu_percent = 50
disk_mb = 1024

[backup]
postgres = true
dirs = ["notes"]

[update]
strategy = "replace"
```

The full Vyasa 0.2.0 manifest (spec §2) is
`crates/manifest/tests/corpus/vyasa_0_2_0.toml`.

## Rules per section

- `schema`: currently `1`.
- `[app]`: `id` matches `[a-z][a-z0-9-]{1,31}`; `version` is semver
  and matches `^[0-9A-Za-z.+-]{1,64}$`; `health` starts with `/`;
  `admin_path` is the link shown after provisioning;
  `startup_grace_s` (default 90, 1–3600) is how long health may fail
  after a start.
- `[run]`: `kind = "binary"` (or `"image"`, parsed but rejected at
  provisioning in this version); `workdir` is the unit's working
  directory: `"exec_dir"` (default, the directory holding `exec`),
  `"bin"` (`bin/current`) or `"data"`; `args` contain no control
  characters.
- `[run.binary."<target>"]`: `url`, `sha256` (64 hex chars) and `exec`,
  a relative path inside the archive matching
  `^[A-Za-z0-9._-]+(/[A-Za-z0-9._-]+)*$` with no `..`;
  `x86_64-linux` is mandatory (`aarch64-linux` is accepted).
- `[needs]`: optional `postgres = ">=16"` (one database + role per
  site) and `dirs`, each matching `^[a-z][a-z0-9-]{0,31}$`.
- `[env]`: names match `^[A-Z][A-Z0-9_]*$`; every entry is exactly one
  of:
  - `generate`: `{ generate = "hex32" | "password20", secret = true }`
    (`hex32` is 64 hex chars, `password20` 20 chars of `[A-Za-z0-9]`);
  - `from`: `{ from = "bind_addr" | "port" | "postgres_url" | "dir" |
    "run_dir" | "owner_email" | "site_url" }`; `bind_addr` is
    `127.0.0.1:<port>`; `postgres_url` is always treated as secret;
    `from = "dir"` needs `dir = "<name>"` listed in `needs.dirs`;
  - `value`: `{ value = "…" }` (a constant, no control characters);
  - `prompt`: `{ prompt = "Label", secret = true|false }` (asked at
    site creation).
  `once = true` requires `secret = true`: the value is written for the
  first boot only, removed from the env file after the first healthy
  check, kept sealed by the control plane, and never re-emitted by an
  update, rollback or env edit.
- `[limits.default]`: `memory_mb`, `cpu_percent` (100 = one core),
  `disk_mb`; the plan's limits override them.
- `[backup]`: `postgres` (bool), `dirs` (a subset of `needs.dirs`).
- `[update]`: `strategy = "replace"`, the only strategy in this version
  (`"command"` is refused). On update, generated and prompt values are
  kept, `value` and `from` entries are re-resolved, new entries are
  resolved fresh and removed ones dropped.

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
