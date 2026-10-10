# Setu

A control panel for single-binary web apps: pick an app, get a domain,
a database, TLS, backups, updates and resource limits, on servers you
own. One binary (`setu`), one Ubuntu 24.04 box to start.

## Documentation

- Install: [INSTALL.md](INSTALL.md) — `install.sh`, then the browser
  wizard at `https://<panel host>/setup` with the printed setup token.
- Operations: [OPERATIONS.md](OPERATIONS.md) — backups and the
  scheduler, backing up and restoring the control plane, upgrading,
  changing the TLS mode, rotating a server token, rebuilding a server.
- App manifests: [MANIFEST.md](MANIFEST.md) — the only app-specific
  knowledge Setu has.
- Agents: [AGENT.md](AGENT.md) — `setu agent install|install-system|join|run|doctor`,
  `state.json`, reconcile and the agent protocol.
- Security model: [SECURITY.md](SECURITY.md) — trust model, the
  privileged helper, what hosted sites share, secrets at rest.
- Cutting a release: [RELEASING.md](RELEASING.md).
- End-to-end and installer tests: [E2E.md](E2E.md).
- Design: [spec](specs/2026-10-09-setu-design.md) (revision 4, as
  built) and its [review record](specs/2026-10-09-setu-review.md). The
  phase 1 plan and its ledger of rulings (the "R70"-style ids) live in
  the private planning repository.
- Later phases: [ROADMAP.md](ROADMAP.md).

## How it works

`setu serve` is the control plane (REST API, embedded web UI, job
runner and scheduler, its own Postgres), listening on a unix socket
behind Caddy. `setu agent run` supervises sites on a managed server as
systemd units, one system user per site, with a local Postgres and
Caddy in front; its only root access is a typed helper behind sudo.
The control plane talks to agents only through long-polled commands;
every command is idempotent, so retries are always safe.

```text
browser → Caddy (TLS) → setu serve → agents → setu-site@<short>.service
                                          → Postgres (one db per site)
                                          → Caddy routes (one per domain)
```

## Five minutes

On a fresh Ubuntu 24.04 VPS with a DNS name pointing at it, as root:

```sh
curl -fsSLO https://github.com/setucp/setu/releases/latest/download/install.sh
bash install.sh --panel-host setu.example.com
```

Open the printed `https://setu.example.com/setup`, paste the printed
setup token, create the owner, and run the shown `setu agent install …`
line with the join token in `SETU_JOIN_TOKEN`. Then
**New site → app → domain** gives a working app with TLS, Postgres
and backups.

## Status

Phase 1 (0.1.0) is built on branch `mvp`, not yet released: single
server or a small fleet, native binary runtime, local backups, updates
with rollback. Roadmap for later phases: [ROADMAP.md](ROADMAP.md).
