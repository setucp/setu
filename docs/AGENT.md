# Agents

One agent per managed server (`setu-agent.service`). It supervises
sites as systemd units, manages the local Postgres, writes Caddy's
config through its admin API, and streams logs and metrics. The agent
keeps `/var/lib/setu/state.json` (sites it knows, ports, db names)
and on start reconciles systemd and Postgres against it, then reports
a full inventory. The control plane is the source of truth and never
SSHes: it only queues typed, idempotent commands the agent long-polls
for.

## Install

On the server, as root, with the one-time join token from
**Servers → Add server** (or the setup wizard's last step):

```sh
setu agent install --control-plane https://setu.example.com --join-token setu_join_… \
  [--tls internal] [--panel-host setu.example.com [--upstream unix//run/setu/http.sock]]
```

Idempotent: adds Caddy's official apt repository and installs `sudo
gnupg postgresql-16 caddy nftables zstd`; creates user `setu` and
`/var/lib/setu` (0750), `/var/lib/setu/{downloads,backups}`,
`/srv/setu/sites` (root 0755); runs `REVOKE CONNECT ON DATABASE postgres
FROM PUBLIC`; then `install-system`:

- the site template unit, `setu-agent.service` and
  `/etc/sudoers.d/setu` (0440, checked with `visudo -cf`);
- the firewall: `setu-nftables.service` loads `/etc/setu/nftables.conf`,
  which replaces only `table inet setu` (never `flush ruleset`) and drops
  non-loopback TCP to ports 20000–29999, so sites are reachable only
  through Caddy;
- Caddy: `caddy.service` is disabled; `caddy-api.service` gets a drop-in
  running `caddy run --environ --config /etc/caddy/setu.json --resume`.
  The admin API listens only on `unix//run/caddy/admin.sock|0660` (group
  `caddy`, which `setu` joins along with `systemd-journal`); TCP 2019 is
  off. Server `setu` listens on `:443` with automatic HTTPS (and the
  HTTP→HTTPS redirect); `--tls internal` uses Caddy's internal CA. A
  re-run applies a new base through the socket (`POST /load` merged
  with the running routes), so site routes survive.

It then joins the control plane, writes `/etc/setu/agent.toml` (server
token, mode 0600, owner `setu`) and starts `setu-agent.service`.
`setu agent install-system [--caddy-only] [--tls internal] [--panel-host
H --upstream U]` runs the system part alone (root; it is not a helper
verb, so sudo cannot reach it).

Routes are Caddy objects with `@id` `setu-site-<short>` (upstream
`127.0.0.1:<port>`) or `setu-panel`; the agent adds with `POST
…/servers/setu/routes`, replaces with `PATCH /id/<id>`, removes with
`DELETE /id/<id>`.

## Commands

- `setu agent run` — long-poll `GET /agent/v1/commands?wait=30`,
  handle, post the result; heartbeat every 15 s (load, free memory
  and disk, unit states, 1-minute metrics batch). Reconnects with
  1 s → 30 s backoff.
- `setu agent join --control-plane URL --join-token T` — redeem a
  join token without reinstalling.
- `setu agent doctor` — checks each dependency (systemd, Postgres,
  Caddy admin socket reachable, TCP 2019 closed, `nft list table inet
  setu`, state file, template unit, token, control plane), prints a
  table, exits 1 on failure.

## Protocol (`/agent/v1`)

Bearer server tokens, never sessions. `POST /join` (single-use join
token, 1-hour expiry → long-lived server token), `GET /commands`,
`POST /commands/{id}/result`, `POST /heartbeat`. Server status stays
`pending` until the first join, then is computed from `last_seen_at`
at read time (`online` within 60 s, else `offline`).

## Privilege model

The agent runs as `setu`; each site runs as its own
`setu-site-<short>` user (uid 30000–39999) under
`ProtectSystem=strict`, `NoNewPrivileges=yes`, with memory/CPU limits
from the plan. The only root path is `sudo -n /opt/setu/setu helper
<verb> --site <short>` with typed JSON on stdin (spec §4). The verbs are
`site-create`, `write-unit`, `write-env`, `install`, `switch-version`,
`unit`, `backup`, `restore` and `site-remove`. Nothing path-like, no
mode and no owner crosses sudo: the helper derives every path from the
short id, opens each component `O_NOFOLLOW`, writes temp-file-plus-
rename, never sets special mode bits, and refuses archives containing
links, devices, `..`, absolute names or setuid/setgid/sticky bits.
Postgres admin is confined to `psql`, `pg_dump` and `pg_restore` as
the `postgres` user. The sudoers file is the two-line golden asset
`crates/agent/assets/sudoers`; `scripts/check-sudoers.sh` asserts it
and runs `visudo -cf`. Logs are read with `journalctl` as `setu`
(group `systemd-journal`), not through the helper.

## What a site looks like

`setu-site@<short>.service` from the template unit, with
`EnvironmentFile=/srv/setu/sites/<short>/env` (root:root 0600, read by
PID 1) and a drop-in `setu-site@<short>.service.d/10-setu.conf` holding
`ExecStart=…/bin/current/<exec> <args>` (systemd-quoted),
`WorkingDirectory=`, `MemoryMax=` and `CPUQuota=`. The agent downloads
to `/var/lib/setu/downloads/<sha256>.tar.gz` (verified); the helper
re-hashes it and extracts into a staging dir renamed to
`bin/<version>/`; `switch-version` swaps the `bin/current` symlink
atomically.
