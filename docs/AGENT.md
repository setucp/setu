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
setu agent install --control-plane https://setu.example.com --join-token setu_join_…
```

Idempotent: creates user `setu`, directories `/var/lib/setu` and
`/srv/setu/sites`, installs `postgresql-16` and `caddy` via apt when
missing (with Caddy's admin API on `127.0.0.1:2019`), writes
`/etc/sudoers.d/setu` from the golden asset, writes and starts
`setu-agent.service`, joins the control plane, and writes
`/etc/setu/agent.toml` (server token, mode 0600).

## Commands

- `setu agent run` — long-poll `GET /agent/v1/commands?wait=30`,
  handle, post the result; heartbeat every 15 s (load, free memory
  and disk, unit states, 1-minute metrics batch). Reconnects with
  1 s → 30 s backoff.
- `setu agent join --control-plane URL --join-token T` — redeem a
  join token without reinstalling.
- `setu agent doctor` — checks each dependency (systemd, Postgres,
  Caddy admin, disk, token file), prints a table, exits 1 on failure.

## Protocol (`/agent/v1`)

Bearer server tokens, never sessions. `POST /join` (single-use join
token, 1-hour expiry → long-lived server token), `GET /commands`,
`POST /commands/{id}/result`, `POST /heartbeat`. Server status stays
`pending` until the first join, then is computed from `last_seen_at`
at read time (`online` within 60 s, else `offline`).

## Privilege model

The agent runs as `setu`; each site runs as its own
`setu-site-<short>` user under `ProtectSystem=strict`,
`NoNewPrivileges=yes`, with memory/CPU limits from the plan. The only
root path is the pinned helper (`setu helper …` with a trailing ` *`:
sudo compares the full command line, and per-site units vary per call,
so the allowlist lives in the helper itself — verbs, units, paths and
usernames, all unit-tested in `helper.rs`). Postgres admin is confined
to `psql`, `pg_dump` and `pg_restore` as the `postgres` user. No
shells, no editors, no package managers. The sudoers file is the golden
asset (`crates/agent/assets/sudoers`); `scripts/check-sudoers.sh`
asserts it matches and `visudo -cf` accepts it. The privileged file
writer is `setu helper write-file --path --mode --owner`, and the path
must be under `/srv/setu/sites/` or the `setu-site@` unit paths.

## What a site looks like

`setu-site@<site-id>.service`: `WorkingDirectory=/srv/setu/sites/%i`,
`EnvironmentFile=…/env` (0600, owned by the site user),
`ExecStart=…/bin/current/<exec> <args>`, `Restart=on-failure`, limits
in a drop-in (`MemoryMax`, `CPUQuota`). Downloads stream to
`bin/<version>/.tmp`, verify sha256, extract, atomic rename;
`SwitchVersion` repoints the `bin/current` symlink.
