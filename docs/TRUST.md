# Trust model, in plain words

Setu runs other people's binaries as services on your servers, and it
can do that only because one part of it is allowed to act as root. This
page says exactly which part, what it can do, and what it cannot. The
technical detail is in [SECURITY.md](SECURITY.md).

## Who can do what

| Component | Runs as | Can |
|---|---|---|
| `setu serve`, the control plane | user `setu` on the panel box | decide what should exist: sites, databases, routes, backups |
| `setu agent`, one per server | user `setu`, no root | carry out the control plane's commands by asking the helper, program Caddy through a socket, run `psql` as the `postgres` user |
| `setu helper`, same binary | root, only through `sudo -n` | nine fixed verbs: create a site user and directories, write a unit drop-in, write an env file, install a verified archive, switch the current version, start/stop/show a unit, stream a backup, restore one, remove a site |
| a hosted site | its own user `setu-site-<id>` | read and write its own `data/` and `run/`, bind its loopback port, nothing else |

The honest summary: **whoever controls the control plane controls
every joined server, at root, within the helper's verbs.** That is the
same trust any control plane asks for. What Setu does is make the root
surface small, typed and auditable, so a bug in the agent or a
compromised agent process cannot turn into "run anything as root".

## What the helper refuses

Every verb takes only a validated eight-character site id on the
command line and typed JSON on standard input. It never accepts a path,
a mode or an owner from its caller. It derives every path from the id,
walks directories without following symlinks, writes through a
temporary file and a rename inside root-owned parents, and never sets
setuid, setgid or sticky bits. Archives are checked entry by entry:
anything with `..`, an absolute name, a symlink, a hard link, a device
or a special bit makes the whole install fail with nothing left behind.
The `--root` test override does not exist in release builds and is
refused under `sudo` even in test builds.

The sudoers file is these two lines and nothing else:

```
setu ALL=(root) NOPASSWD: /opt/setu/setu helper *
setu ALL=(postgres) NOPASSWD: /usr/bin/psql, /usr/bin/pg_dump, /usr/bin/pg_restore
```

`helper *` is safe because the verbs take no free-form arguments.
`psql` as `postgres` is a database superuser; Setu only passes it
scripts on standard input and only for names matching `site_<id>`,
and the file says so because it is the one place where more than Setu
intended could happen.

## What sites can and cannot reach

- They cannot reach Caddy's admin API: it listens on a unix socket owned
  by group `caddy`, TCP 2019 is off, and `setu agent doctor` checks both.
- They cannot be reached from outside on their port: an nftables rule
  drops inbound 20000–29999 on every interface but loopback, so traffic
  comes only through Caddy.
- They cannot connect to other sites' databases: every database revokes
  `CONNECT` from `PUBLIC`.
- They **do** share the kernel, the Postgres instance and loopback with
  each other. A hosted app can reach another site's port on 127.0.0.1,
  and an app that trusts `X-Forwarded-For` from 127.0.0.1 can be fooled
  by a neighbour. Phase 1 is process isolation, not VM isolation; the
  roadmap has containers for tenants who need more.

## Secrets

Database passwords and generated secrets are sealed with ChaCha20-
Poly1305 under the panel's key before they are stored, never written to
logs, never passed on a command line (psql reads them on standard
input), and blanked out of the command queue as soon as a command
finishes, is cancelled or expires. Operators see masked values; a
reveal needs the operator's password and is written to the audit log
every time. Application logs are returned as the app wrote them, so an
app that prints its own secrets exposes them to operators.

## What Setu proves before a release

Every release runs an end-to-end test on a real systemd in a container:
provision, firewall, Caddy admin closed, limits, backup and restore, an
update that migrates and fails health and must roll back with the
database restored, an agent killed mid-command, delete, a real Vyasa
site, a reboot, and a lost Caddy config repaired. The installer has its
own test. See [E2E.md](E2E.md).

## Reporting a vulnerability

Use GitHub's private vulnerability reporting on the repository
(Security → Report a vulnerability). Please do not open a public issue
for anything that could be exploited. See [`SECURITY.md`](../SECURITY.md)
at the repository root for the policy.
