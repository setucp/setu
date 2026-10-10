# Agents

One agent per managed server (`setu-agent.service`, user `setu`). It
supervises sites as systemd units, manages the local Postgres, writes
Caddy's config through its admin socket, and streams logs and metrics.
The control plane is the source of truth and never SSHes: it queues
typed, idempotent commands the agent long-polls for (spec §4, §5).

## Install

On the server, as root, with the one-time join token from
**Servers → Add server** (or the setup wizard's last step; the panel
shows the install command and the token separately):

```sh
SETU_JOIN_TOKEN=setu_join_… setu agent install --control-plane https://setu.example.com
```

| Flag | Meaning |
|---|---|
| `--control-plane <url>` | the panel's base URL (required). `https://` always; `http://` only to a loopback address or `localhost`, anything else only with `--insecure` |
| `--insecure` | allow plain `http://` to a non-loopback control plane (tests). It never disables certificate checks |
| `--ca <file>` | a PEM root certificate to trust for the control plane (e.g. Caddy's internal CA); copied to `/etc/setu/ca.pem` 0644 and recorded in `agent.toml` |
| `--tls internal` | this server's Caddy issues certificates from its internal CA instead of ACME |
| `--panel-host <host>` | also route `<host>` to the panel (the control-plane box); recorded in `agent.toml` and reconciled as route `setu-panel` |
| `--upstream <u>` | the panel upstream for `--panel-host` (default `unix//run/setu/http.sock`) |
| `--config <path>` | where to write the agent config (default `/etc/setu/agent.toml`) |

The join token comes from `SETU_JOIN_TOKEN` or, when unset, the first
line of stdin; it is never accepted on the command line (`ps` and shell
history would show it). The agent draws one random join nonce per
install and retries the join once, with the same nonce, only when no
HTTP response arrived, so a lost response is recoverable (the control
plane returns the same server token for the same nonce within 60 s).

What `agent install` does, idempotently and in this order:

1. **Packages**: adds Caddy's official apt repository and installs
   `sudo gnupg postgresql-16 caddy nftables zstd`. If any step of the
   Caddy repository fails (x86_64), it removes the repository list,
   installs the other packages from Ubuntu and installs Caddy's own
   release package `caddy_2.11.7_linux_amd64.deb`, checked against a
   pinned SHA-512 (ledger R116). Then `systemctl enable --now
   postgresql`.
2. **User** `setu` (system, home `/var/lib/setu`, no login shell).
3. **Directories**: `/var/lib/setu`, `/var/lib/setu/downloads`,
   `/var/lib/setu/backups` (`setu` 0750); `/etc/setu`, `/srv/setu`,
   `/srv/setu/sites` (`root` 0755).
4. **Postgres**: `REVOKE CONNECT ON DATABASE postgres FROM PUBLIC`, and
   on the control plane's `setu` database when it exists.
5. **`install-system`**: the site template unit, `setu-agent.service`,
   `/etc/sudoers.d/setu` (0440, `visudo -cf` before it is installed);
   the firewall (`/etc/setu/nftables.conf` and `setu-nftables.service`,
   enabled and restarted: only `table inet setu`, dropping non-loopback
   TCP to 20000–29999); `setu` joins groups `caddy` and
   `systemd-journal`; Caddy: `caddy.service` disabled,
   `caddy-api.service` with a drop-in running `caddy run --environ
   --config /etc/caddy/setu.json --resume`, admin only on
   `unix//run/caddy/admin.sock|0660` (TCP 2019 off), server `setu` on
   `:443` with automatic HTTPS. It always finishes with `POST /load` of
   the base config merged with the running routes, so a re-run changes
   the TLS mode or panel host without losing site routes.
6. **Join**, then write `/etc/setu/agent.toml`: created 0600 from the
   start (random temp name, fsync, rename), then owned by `setu`.
7. `systemctl enable` and `restart setu-agent.service` (only after the
   join, so the agent never starts without a token).

Other subcommands (all root except `run` and `doctor`):

- `setu agent install-system [--caddy-only] [--tls internal]
  [--panel-host H --upstream U]` — step 5 alone (and the Caddy package
  when it is missing). Not a helper verb, so sudo cannot reach it. Use
  it to change the TLS mode or panel host
  ([OPERATIONS.md](OPERATIONS.md#changing-the-tls-mode-or-the-panel-host)).
- `setu agent join --control-plane URL [--insecure] [--ca F] [--config
  P]` — redeem a join token without reinstalling.
- `setu agent run [--config P]` — the agent itself (what the unit runs).
- `setu agent doctor [--config P]` — see below.

## `agent.toml`

`/etc/setu/agent.toml` (`setu`, 0600): `control_plane`, `server_id`,
`server_token`, `insecure`, `ca`, `panel` (`host`, `upstream`), and
defaults you rarely change: `state_path` (`/var/lib/setu/state.json`),
`sites_root`, `downloads_dir`, `backups_root`, `caddy_socket`
(`/run/caddy/admin.sock`), `poll_wait_secs` (30), `heartbeat_secs`
(15), `backoff_min_secs`/`backoff_max_secs` (1/30), `reconcile_secs`
(300).

Environment variables:

| Variable | Used by | Meaning |
|---|---|---|
| `SETU_JOIN_TOKEN` | `agent install`, `agent join` | the one-time join token (else stdin) |
| `SETU_AGENT_RECONCILE_SECS` | `agent run` | overrides `reconcile_secs` (the e2e uses 10) |
| `RUST_LOG` | every role | log filter, default `info` |
| `SETU_AGENT_TEST_EXIT_AFTER=<kind>` | `agent run`, **only** in builds with the `e2e-hooks` feature | exit after persisting that command's result, before posting it (e2e assertion 10) |

## Running

`setu agent run` long-polls `GET /agent/v1/commands?wait=30`. Each
command runs in its own task after taking its site's FIFO slot, so one
site's 15-minute install never blocks another site; `status`,
`fetch_logs` and `inventory` take no slot, and the database commands
take the slot of the `site_<short>` they name (R137). A
command is rejected with `expired` once its `expires_at` has passed
(checked again when it reaches the front of its site's queue), and
deduped by id against the last 1000 results. Its result is written to
`state.json` **before** it is posted; results never acknowledged are
re-posted on start and after a failed post. Secret values a command
carried (`create_database` password, `write_env` values) are replaced
by `***` in its log and error. Poll errors back off 1 s → 30 s.

Heartbeat (its own task, every 15 s): `boot_id` (kernel boot id plus a
per-process id, so an agent restart lets the control plane requeue
commands it had delivered), load, free memory and disk, unit states,
and per-site CPU and memory from the helper's `unit show` every beat.
Per-site data size (`du -sb data/`) is measured at most every ten
minutes and reused in between (`DATA_BYTES_TTL`, #5).

### `state.json`

`/var/lib/setu/state.json` (`setu` 0600, written temp-file + fsync +
rename under one in-process lock):

- `sites`: by short id, `{domain, port, version, db}`: what the agent
  last applied (`SetRoute` records domain and port before touching
  Caddy, `RemoveRoute` clears the domain, `SiteRemove` drops the
  record);
- `recent`: up to 1000 `{id, result, posted, read_only}`; read-only
  results leave once posted, so a redelivered `status` or `fetch_logs`
  is simply re-run.

### Reconcile

On start and every `reconcile_secs`: every site with a recorded domain
and port gets its route `setu-site-<short>` → `127.0.0.1:<port>`
(upsert: `PATCH /id/<id>` when present, else `POST
…/servers/setu/routes`), plus `setu-panel` from `agent.toml`;
`setu-site-*` routes whose site is unknown or has no domain are
removed; routes Setu did not write are never touched. Site trees under
`/srv/setu/sites` and `site_*` databases the state does not know are
logged as warnings, never deleted. This is how a Caddy that lost its
autosave gets its routes back (e2e assertion 14).

## `doctor`

`setu agent doctor` prints one row per check and exits 1 on any
failure:

| Row | Check |
|---|---|
| `helper` | `/opt/setu/setu` is a regular file, root:root, mode exactly 0755, in a root-owned directory without group/other write |
| `sudoers` | `visudo -cf /etc/sudoers.d/setu` |
| `site-template` | `/etc/systemd/system/setu-site@.service` exists |
| `protected-hardlinks` | sysctl `fs.protected_hardlinks` is `1` (Ubuntu's default) |
| `caddy-api` | `systemctl is-active caddy-api.service` |
| `caddy-socket` | the admin socket answers |
| `caddy-tcp-2019` | nothing listens on `127.0.0.1:2019` |
| `nftables` | `nft list table inet setu` |
| `postgres` | `sudo -n -u postgres psql -X -c 'select 1'` (peer auth) |
| `state-file` | `state.json` loads |
| `server-token` | `agent.toml` holds a token |
| `control-plane` | `GET <control_plane>/healthz` answers 2xx |

## Protocol (`/agent/v1`)

Bearer server tokens, never sessions. `POST /join` (single-use join
token, 1-hour expiry, plus the nonce → server id and long-lived token),
`GET /commands?wait=N` (N ≤ 30; `wait=0` checks once and returns 204 at
once; 600 polls per minute per server), `POST /commands/{id}/result`,
`POST /heartbeat`. Server status stays `pending` until the join, then
is computed at read time (`online` within 60 s of the last heartbeat,
else `offline`).

## Privilege model

Each site runs as its own `setu-site-<short>` user (uid 30000–39999)
under `ProtectSystem=strict`, `NoNewPrivileges=yes` and the plan's
memory and CPU limits. The agent's only root path is `sudo -n
/opt/setu/setu helper <verb> --site <short>` with typed JSON on stdin;
the verbs are `site-create`, `write-unit`, `write-env`, `install`,
`switch-version`, `unit`, `backup`, `restore` and `site-remove`.
Postgres admin is `psql`, `pg_dump` and `pg_restore` as `postgres`.
Logs are read with `journalctl` as `setu` (group `systemd-journal`),
not through the helper. The full model, verb table and sudoers file
are in [SECURITY.md](SECURITY.md#the-helper).

`setu-agent.service` runs as `setu` with `Restart=always` and without
`NoNewPrivileges` (it needs sudo) or `ProtectSystem=` (sudo children
share its mount namespace, and the helper writes under `/etc`).

## What a site looks like

`setu-site@<short>.service` from the template unit, with
`EnvironmentFile=/srv/setu/sites/<short>/env` (root:root 0600, read by
PID 1) and a drop-in `setu-site@<short>.service.d/10-setu.conf` holding
`ExecStart=…/bin/current/<exec> <args>` (systemd-quoted),
`WorkingDirectory=`, `MemoryMax=` and `CPUQuota=`. Starting a site
enables it and stopping disables it, so its state survives a reboot.
The agent downloads to `/var/lib/setu/downloads/<sha256>.tar.gz`
(verified); the helper re-hashes it and extracts into a staging dir
renamed to `bin/<version>/`; `switch-version` swaps the `bin/current`
symlink atomically. Backups land in
`/var/lib/setu/backups/<short>/<backup_id>/{db.dump,dirs.tar.zst}`.
