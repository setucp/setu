# Setu

A control panel for single-binary web apps: pick an app, get a domain,
a database, TLS, backups, updates and resource limits, on servers you
own. One binary (`setu`), one Ubuntu 24.04 box to start.

- Install: [INSTALL.md](INSTALL.md) — `install.sh`, then the browser
  wizard at `http://<server>:7000/setup`.
- App manifests: [MANIFEST.md](MANIFEST.md) — the only app-specific
  knowledge Setu has.
- Agents: [AGENT.md](AGENT.md) — `setu agent install|run|doctor` and
  the agent protocol.
- Security model: [SECURITY.md](SECURITY.md).
- Cutting a release: [RELEASING.md](RELEASING.md).
- End-to-end test: [E2E.md](E2E.md).

## How it works

`setu serve` is the control plane (REST API, embedded web UI, job
runner, its own Postgres). `setu agent run` supervises sites on a
managed server as systemd units, one system user per site, with a
local Postgres and Caddy in front. The control plane talks to agents
only through long-polled commands; every command is idempotent, so
retries are always safe.

```text
browser → Caddy (TLS) → setu serve → agents → setu-site@<id>.service
                                          → Postgres (one db per site)
                                          → Caddy routes (one per domain)
```

## Five minutes

On a fresh Ubuntu 24.04 VPS, as root:

```sh
curl -fsSL https://github.com/vyasa-cms/setu/releases/latest/download/install.sh | sh
```

Open the printed `http://<ip>:7000/setup`: create the owner, register
the server, run the shown `setu agent install …` line. Then
**New site → app → domain** gives a working app with TLS, Postgres
and backups.

## Status

MVP (0.1.0, unreleased): single server, native binary runtime,
local backups, updates with rollback. Roadmap for later phases:
[ROADMAP.md](ROADMAP.md). Design: [spec](specs/2026-10-09-setu-design.md).
