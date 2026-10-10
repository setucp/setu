# Security model

What Setu protects, from whom, and what it leaves to you. The design is
spec §4 and §7 ([spec](specs/2026-10-09-setu-design.md)); ruling ids
like (R70) point into the phase 1 ledger of rulings (kept in the private
planning repository).

## Trust model

- **The control plane is root on every agent.** An agent executes the
  typed commands its control plane queues, and some of them run as root
  through the helper. Whoever controls the control plane (its host, its
  database, its `secret_key`, an `owner` or `operator` account) can
  create users, write units and env files, install binaries and drop
  databases on every joined server, within the helper's verbs. The
  helper bounds *what* can be done (no free paths, modes, owners or
  shell), not *who* asks. Protect the control plane accordingly.
- **The agent is not root.** It runs as `setu` and reaches root only
  through `sudo -n /opt/setu/setu helper <verb> --site <short>` and
  `postgres` only through `psql`/`pg_dump`/`pg_restore`. A compromised
  agent process is bounded by the same verbs.
- **Sites are untrusted.** Each runs as its own user under a hardened
  unit and cannot write anything a privileged path later reads (only its
  `data/` and `run/`).
- **Operators** are authenticated people with a role; `viewer`s never
  see a secret.
- On the single-box install the control plane and the agent run as the
  same `setu` user, so a compromise of either is a compromise of both.

## The helper

`/opt/setu/setu` is root:root 0755 in a root-owned directory (`setu
agent doctor` checks both). The agent calls it only as `sudo -n
/opt/setu/setu helper <verb> --site <short>`. Guarantees:

- The only arguments are the verb and `--site`, validated as
  `^[a-z0-9]{8}$`. Input is one line of typed JSON on stdin (unknown
  fields refused, at most 1 MiB); archives stream over stdin/stdout
  (R61). Nothing path-like, no mode and no owner crosses sudo.
- Every path derives from the short id. Each component under
  `/srv/setu/sites` is opened `O_NOFOLLOW|O_DIRECTORY` step by step from
  root; files are written temp-file-plus-rename inside root-owned
  parents; no special mode bit is ever set.
- The helper re-validates what it is given: `version`
  `^[0-9A-Za-z.+-]{1,64}$`, `exec` relative `[A-Za-z0-9._-]` segments
  without `..`, `args` and env values without control characters, dir
  names `^[a-z][a-z0-9-]{0,31}$`, env names `^[A-Z][A-Z0-9_]*$`, sha256
  64 lowercase hex. Unit values are systemd-escaped; env values use
  systemd's `EnvironmentFile=` quoting (R64).
- Archives (`install`, `restore`) are extracted into a root-owned
  staging directory; any `..`, absolute name, symlink, hard link, device
  file or setuid/setgid/sticky bit refuses the whole operation and
  leaves nothing behind. `backup` never follows a link a site user
  planted in `data/` (links and special files are skipped), and skips
  regular files the site user does not own or that have more than one
  hard link (R135); `agent doctor` checks `fs.protected_hardlinks = 1`.
- `--root <dir>` (an alternate filesystem root for tests) exists only
  in builds with the `test-root` feature, is refused when `SUDO_UID` is
  set, and the release workflow fails if the binary offers it (R129).
- `install-system` (units, sudoers, firewall, Caddy) is not a helper
  verb; it runs only as real root from `setu agent install`, `setu agent
  install-system` and `install.sh`.

| verb | what root does |
|---|---|
| `site-create {dirs}` | `useradd setu-site-<short>` (uid 30000–39999, no shell); site tree root 0755, `data/`, `run/`, `data/<dir>` site user 0750 |
| `write-unit {short_id, version, exec, args, workdir, memory_mb, cpu_percent}` | the drop-in `setu-site@<short>.service.d/10-setu.conf` from a fixed template; `daemon-reload` when changed |
| `write-env [{name, value}]` | `/srv/setu/sites/<short>/env`, root:root 0600 |
| `install {version, sha256[, format, exec]}` | verify `/var/lib/setu/downloads/<sha256>.tar.gz` (the name is kept for a raw binary too), then extract it to `bin/<version>/`, or with `format: "binary"` copy it to `bin/<version>/<exec>` (`exec` one file name), 0755 |
| `switch-version {version}` | atomic swap of the `bin/current` symlink |
| `unit {op}` | `start` = `systemctl enable --now`, `stop` = `disable --now`, `restart` = `enable` + `restart`, `show` = unit properties + `du -sb data/` (R118); `data_bytes: false` (bool, default true) skips the `du` walk (#5) |
| `backup {dirs}` | `tar.zst` of `data/<dir>`… to stdout |
| `restore {dirs}` + stream | unit must be inactive; staging, chown to the site user, swap into `data/` |
| `site-remove {}` | `disable --now`, remove the drop-in, `userdel`, remove the tree |

### sudoers

`/etc/sudoers.d/setu` (0440), verbatim (R70):

```
setu ALL=(root) NOPASSWD: /opt/setu/setu helper *
setu ALL=(postgres) NOPASSWD: /usr/bin/psql, /usr/bin/pg_dump, /usr/bin/pg_restore
```

`helper *` is safe because every verb takes only `--site <short>` and
typed stdin. `psql` receives SQL on stdin; a role password is a
`\set pw '<alphanumeric>'` line on stdin, never argv or the sudo log.
`install-system` checks the file with `visudo -cf` before installing it;
`scripts/check-sudoers.sh` compares the golden asset and runs `visudo
-cf` (classic sudo and sudo-rs both accept it).

## Network

| Rule | Where |
|---|---|
| Inbound non-loopback TCP to 20000–29999 dropped (site ports) | `table inet setu` from `/etc/setu/nftables.conf`, loaded by `setu-nftables.service`; never `flush ruleset`, other tables untouched |
| Caddy serves `:443` with automatic HTTPS (ACME, or its internal CA with `--tls internal`) and the HTTP→HTTPS redirect on `:80` | `/etc/caddy/setu.json` |
| Caddy's TCP admin (2019) is off | `doctor` and e2e assertion 5 check it is closed |
| The panel has no TCP listener on an installer box | `setu serve` listens on `/run/setu/http.sock` |
| Agents connect out to the control plane over HTTPS; nothing connects in to an agent | — |

Setu manages only its own table. SSH, Postgres (Ubuntu's default listens
on localhost only) and everything else on the host are yours to
firewall.

## Caddy's admin socket

Caddy is programmed only through `unix//run/caddy/admin.sock|0660`,
group `caddy`; `setu` is in that group, site users are not (e2e
assertion 5 checks a site user cannot open it). Anyone who can write
that socket controls every route and certificate on the box. Routes
Setu writes have ids `setu-site-<short>` and `setu-panel`; others are
never touched.

The panel socket `/run/setu/http.sock` is 0660 `setu:caddy`, so only
Caddy (and `setu`) can connect. Its directory is 0755 (R126): local
users can see the socket's name, not use it.

## What hosted sites share

Sites are isolated by user, unit hardening (`ProtectSystem=strict`,
`ProtectHome`, `PrivateTmp`, `NoNewPrivileges`, `RestrictSUIDSGID`,
`UMask=0027`) and cgroup limits, but they share:

- **the kernel**: a kernel exploit in one site is every site's;
- **the Postgres instance**: each site has its own role and database
  with `REVOKE CONNECT … FROM PUBLIC` (a test asserts another site's
  role cannot connect), but one site can load the shared server;
- **loopback**: any site can connect to another site's port on
  127.0.0.1 (the firewall only blocks other interfaces), and can send
  forged `X-Forwarded-For` headers to apps that trust 127.0.0.1 as a
  proxy. Vyasa does (`VYASA_TRUSTED_PROXIES = "127.0.0.1"`), so a
  hostile neighbour can make its requests appear to come from any IP
  (rate limits, audit IPs). Host mutually distrusting customers on
  separate servers until phase 2 adds network isolation;
- **disk**: `disk_mb` is reported, not enforced, in phase 1.

The same applies to the panel when it listens on TCP: with the default
`trusted_proxies = ["127.0.0.1/32"]` a site on the same box can reach
`127.0.0.1:7000` and forge the client IP the login limiter and the
audit log see. The installer's unix socket avoids this.

## Application logs

`FetchLogs` (the site's Logs tab) returns the app's journal **verbatim**.
Setu redacts the secrets it handles in its own job logs and command
results, but it cannot know what an app prints: apps must not log their
own secrets (database URLs, generated keys, the one-time admin
password).

## Operators and sessions

- Passwords are argon2id. An unknown email is verified against a dummy
  hash, so its timing and its 401 match a wrong password.
- Login rate limit: 10 failures per client IP per 15 minutes → 429
  `rate_limited`. Wrong passwords on reveal count in the same limiter.
  An unknown client IP shares one bucket.
- Sessions live in the `sessions` table: the cookie `setu_session`
  carries 32 random bytes, only their SHA-256 is stored; `HttpOnly`,
  `SameSite=Lax`, `Secure` when `public_url` is https; 7 days, sliding.
  Login always issues a fresh session (no fixation); a password change
  ends every session of that operator (R102).
  Deleting an operator (owner only) refuses yourself and the last
  owner with 409, ends every session of theirs and is audited as
  `operator.delete`; concurrent deletions are serialised by an advisory
  lock, so two owners deleting each other cannot both succeed (R149).
- Roles: `owner` > `operator` > `viewer`; every route × role is pinned
  by `crates/api/tests/matrix.rs`. Every mutation writes `audit_log`
  (operator, action, target kind and id, client IP).

### The Origin rule

Every `POST`/`PUT`/`DELETE`/`PATCH` under `/api/v1`, login and setup
included, must carry `Origin` equal to `public_url` (scheme, host and
port) or `Sec-Fetch-Site: same-origin`; anything else is 403 (R103).
Browsers send one of them; scripts must send `Origin: <public_url>`.
The agent API (`/agent/v1`, bearer tokens) is outside this rule.

### Client IP and X-Forwarded-For

The client IP feeds the login limiter and audit rows (R104):

- on the unix socket the peer is Caddy by construction, so
  `X-Forwarded-For` is believed;
- on TCP it is believed only when the peer is in `trusted_proxies`
  (CIDRs; default `127.0.0.1/32`); otherwise the peer address is the
  client;
- within a believed header the client is the rightmost hop that is not
  itself a trusted proxy (a client cannot hide behind hops it prepended);
  if every hop is trusted, the leftmost; an unparsable hop voids the
  header (unix peer: unknown IP; TCP: the peer).

### Revealing a secret

`GET /sites/{id}/env` masks every secret value for every role, and any
value sourced from `postgres_url` regardless of its flag. The only way
to read one is `POST /sites/{id}/env/{name}/reveal {password}`:
`operator` or `owner`, with the caller's own password. A wrong password
is 403 and counts in the login limiter. Every attempt past the role
check is audited: `env.reveal` when the value was returned,
`env.reveal_denied` when not, target `<site_id>:<NAME>` (R107).

### The setup token

With no operator in the database, `setu serve` writes a one-time token
to `<data_dir>/setup-token` (0600 from creation, readable only by
`setu` and root) and stores only its SHA-256. `POST /setup` needs it;
the token check, the owner insert and the hash deletion are one
transaction under an advisory lock, so exactly one owner is created;
the file is then removed. A lost file with no owner yet makes the next
boot mint a new token (R105). After the first owner, `/setup` answers
409.

### The join token and nonce

`POST /servers` (and the wizard) create a pending server with a join
token (`setu_join_…`, single use, 1 hour). It is shown once, next to an
install command that does not contain it; the agent reads it from
`SETU_JOIN_TOKEN` or stdin, never argv. The agent sends a random nonce
with it: a retry within 60 s with the same nonce gets the same server
token back (a lost response is recoverable), a different nonce is
refused, so someone who saw the join token cannot take over a server
that already joined. Every failure is the same 401 (R76, R99).

The server token (`setu_srv_…`, 32 random bytes) is stored as a SHA-256
hash on the control plane and in `/etc/setu/agent.toml` (`setu`, 0600,
created 0600 rather than chmodded later) on the agent. Agent polling is
rate-limited per server. Rotation: [OPERATIONS.md](OPERATIONS.md#rotating-a-server-token).

## Secrets at rest

| Secret | Where | Protection |
|---|---|---|
| `secret_key` (64 hex), database URL | `/etc/setu/setu.toml` | `setu` 0600; the key seals everything below. Losing it loses every sealed value; rotating it is not supported in phase 1 |
| Site env values | `sites.env` | each value sealed: `v1:` = ChaCha20-Poly1305 with a random nonce under SHA-256(`"setu-seal-v1"` ‖ secret key), `plain:` for values not marked secret; an unprefixed value is refused (R51) |
| Old env and spec during an update, prompt answers, env edits | `jobs.payload` | always `v1:` sealed, whatever the `secret` flag; prompt answers nulled once stored (R93, R110) |
| Join retry token | `servers.join_token_sealed` | sealed, nulled 60 s after the join |
| Commands with secrets (`create_database` password, `write_env` values) | `agent_commands.command` | nulled by `setu_redact_command` when the row is completed, cancelled, expired or pruned (R75) |
| Command results and job logs | `agent_commands.result`, `jobs.log` | the agent replaces every password and env value it carried with `***` in `log` and `error` before persisting; job log lines replace every registered secret with `***` (R93, R96) |
| Operator passwords, session ids, server/join/setup tokens | DB | argon2id, SHA-256 hashes |
| Site env file | `/srv/setu/sites/<short>/env` | root:root 0600, read by PID 1 |
| Server token | `/etc/setu/agent.toml` | `setu` 0600 |
| Site backups | `/var/lib/setu/backups/<short>/<backup_id>/` | `setu` 0750, **not encrypted**: the dump holds the app's data and any secrets the app stored |
| Control-plane dump (`setu backup`) | where you write it | holds `v1:` values (useless without `secret_key`) and token hashes |

Every struct that holds a secret has a `Debug` that redacts it; secrets
never travel on argv (join token, operator password, role passwords,
`pg_dump`'s password via `PGPASSWORD` in the child's environment).

## Not in phase 1

- Two-factor authentication and operator API tokens (phase 2).
- Enforced disk quotas (reported only).
- Network isolation between sites on one box.
- Encryption of local backups, off-box backups.
- Rotation of `secret_key` and a command to rotate a server token.
- Telemetry: there is none.
