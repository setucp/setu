<p align="center"><img src="docs/brand/github-banner.png" alt="Setu — a control panel for single-binary web apps" width="100%"></p>

# Setu

### Your binaries. Your servers. One control panel.

Deploy and operate Rust, Go and other compiled web applications on your
own Linux servers. Each site gets a domain, automatic HTTPS, its own
PostgreSQL database, backups, health-checked updates and resource
limits. Nothing to build and no runtime to maintain on the server: the
app is a published binary, and Setu runs it as a hardened systemd
service.

<p>
  <img src="https://img.shields.io/badge/native%20systemd-4F46E5" alt="Native systemd">
  <img src="https://img.shields.io/badge/automatic%20HTTPS-4F46E5" alt="Automatic HTTPS">
  <img src="https://img.shields.io/badge/per--app%20PostgreSQL-4F46E5" alt="Per-app PostgreSQL">
  <img src="https://img.shields.io/badge/rollback%20with%20restore-4F46E5" alt="Rollback with restore">
</p>

<p align="center"><img src="docs/brand/setu-demo.gif" alt="Installing Setu, creating a site from the panel, and the site serving over HTTPS" width="900"></p>

*A real run: the installer, the panel, a Vyasa site provisioned in about ten seconds, live over HTTPS.*

<p align="center"><img src="docs/brand/architecture.png" alt="Setu architecture: control plane, per-server agent, privileged helper, one systemd service per site" width="100%"></p>

<p>
  <a href="https://github.com/setucp/setu/releases/tag/v0.1.0"><img src="https://img.shields.io/badge/release-v0.1.0-4F46E5" alt="Release v0.1.0"></a>
  <a href="https://github.com/setucp/setu/actions/workflows/ci.yml"><img src="https://github.com/setucp/setu/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <img src="https://img.shields.io/badge/licence-AGPL--3.0-blue" alt="Licence: AGPL-3.0">
  <img src="https://img.shields.io/badge/platform-Ubuntu%2022.04%2F24.04%2F26.04%20%C2%B7%20Debian%2012%2F13%20%C2%B7%20x86__64%20and%20aarch64-lightgrey" alt="Platform: Ubuntu 22.04 and 24.04, Debian 12 and 13, x86_64 and aarch64">
</p>

## Why Setu

- **One binary.** `setu` is the control plane, the per-server agent and
  the privileged helper, selected by subcommand. One static musl (x86_64 and aarch64)
  archive per release.
- **Native systemd, real limits.** Every site runs as
  `setu-site@<id>.service` under its own user, with memory and CPU
  limits enforced by cgroups and a hardened unit. No container runtime
  in the path.
- **A small, typed root surface.** The agent is not root. It reaches
  root only through `setu helper <verb> --site <id>`: typed JSON on
  stdin, no paths, modes or owners from the caller. The whole sudoers
  file is two lines.
- **Caddy in front.** Automatic HTTPS through ACME (or Caddy's internal
  CA), programmed through its admin socket, one route per domain.
- **Updates that roll back.** An update backs up first; if the new
  version does not come up healthy, Setu puts the old binary and env
  back and, when the new version already ran, restores the database
  and data directories too.

## Quick start

You need a fresh Ubuntu 22.04 or 24.04, or Debian 12 or 13, server (x86_64 or aarch64), root, ports 80 and 443
open, and a DNS name pointing at the server. The installer brings
PostgreSQL 16 or 17 depending on the distribution, and Caddy itself.

```sh
curl -fsSLO https://github.com/setucp/setu/releases/latest/download/install.sh && sudo bash install.sh --panel-host panel.example.com
```

The script verifies the release archive against `SHA256SUMS`, installs
`/opt/setu/setu`, creates the `setu` user and database, configures
Caddy for the panel host and starts `setu.service`. Then:

1. It prints `https://panel.example.com/setup` and a one-time setup
   token. Open the URL and paste the token.
2. The wizard creates the owner account (this works exactly once).
3. The wizard registers this server: run the `setu agent install …`
   line it shows, as root, with the join token in `SETU_JOIN_TOKEN`.
   The server shows `online` within seconds.
4. **Apps → Add**: paste or link an app manifest (see [Apps](#apps)).
5. **New site → app → domain**: Setu provisions the site and it is
   live with TLS, its own database, limits and a nightly backup.

No DNS name yet? `--tls internal` uses Caddy's internal CA instead of
ACME. All flags, the manual install, upgrading and uninstalling are in
[docs/INSTALL.md](docs/INSTALL.md).

## How it works

```text
                      browser
                         │ HTTPS
   ┌─────────────────────▼──────────────────────────────┐
   │ control-plane box                                  │
   │   Caddy ──unix socket──▶ setu serve                │
   │                          REST API, web UI, jobs,   │
   │                          scheduler ──▶ Postgres    │
   └─────────────────────▲──────────────────────────────┘
                         │ agents long-poll over HTTPS
                         │ (outbound only, per-server token)
   ┌─────────────────────┴──────────────────────────────┐
   │ managed server (the same box, or any other)        │
   │   setu agent run ──sudo -n──▶ setu helper <verb>   │
   │     │                          units, users, env,  │
   │     │                          installs, backups   │
   │     ├─▶ Postgres (one database and role per site)  │
   │     ├─▶ Caddy admin socket (one route per domain)  │
   │     └─▶ setu-site@<id>.service (cgroup limits)     │
   │   nftables: site ports 20000-29999 not reachable   │
   │   from outside                                     │
   └────────────────────────────────────────────────────┘
```

The control plane is the source of truth and never SSHes; it queues
typed, idempotent commands that agents long-poll for, so every retry is
safe. Provisioning a site runs as a resumable job:
validate → create user and dirs → create database → install binary →
switch version → write unit → write env → start → health check → add
Caddy route → verify → finish.

More: [docs/AGENT.md](docs/AGENT.md) (agents and the protocol),
[docs/OPERATIONS.md](docs/OPERATIONS.md) (backups, upgrades, token
rotation), and the [design spec](docs/specs/2026-10-09-setu-design.md).

## Apps

An app is a TOML manifest per version. It is the only app-specific
knowledge Setu has:

```toml
schema = 1

[app]
id = "hello"
name = "Hello"
version = "0.1.0"
homepage = "https://example.com"
health = "/healthz"
admin_path = "/"

[run]
kind = "binary"
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
HELLO_TOKEN = { generate = "hex32", secret = true }
```

Shortened: a complete manifest also needs the `[limits.default]`,
`[backup]` and `[update]` tables, as in the full `hello` and Vyasa
examples.

The contract for app authors: ship one static binary in a tarball,
read the address to bind and everything else from environment variables
the manifest declares, answer `2xx` on the health path when ready, and
keep persistent state only in the declared directories and the
database. Field reference, env rules and examples:
[docs/MANIFEST.md](docs/MANIFEST.md).

## What is in 0.1.0, and what is not yet

In: one server or a small fleet, the native binary runtime, the web UI
and REST API (OpenAPI at `/api/v1/openapi.json`), operators with roles,
sealed secrets, local backups with restore, updates with rollback,
resource limits, Caddy TLS.

Not yet, by design:

- **Containers** as a runtime (phase 2; the manifest already carries
  `run.kind = "image"`).
- **Site moves and server drain**, off-box (S3) backups, enforced disk
  quotas (disk is reported, not enforced).
- **Billing and customers** (phase 3).
- aarch64 builds, a container image of Setu itself, TOTP, operator API
  tokens and a CLI client.

Managed interpreted runtimes (PHP, Node, Python) are out of scope;
apps that need an interpreter will ship as an image. The plan for
later phases is in [docs/ROADMAP.md](docs/ROADMAP.md).

## Security

Start with [docs/TRUST.md](docs/TRUST.md), the trust model in plain words. 
The control plane is effectively root on every server that joins it:
agents carry out the commands it queues, some through the privileged
helper. The helper bounds *what* root can be asked to do (typed verbs,
fixed templates, every path derived from the site id, hostile archives
refused) but not *who* asks, so protect the control plane, its database
and its `secret_key` accordingly. Sites are untrusted: each runs as its
own user and can write only its `data/` and `run/` directories. Sites
on one server share the kernel, the local Postgres instance (one
database and role each) and Caddy. Secrets are sealed at rest, never
put on a command line or in logs, and revealed only with the operator's
password. The full model, the helper's verbs, the sudoers file and the
firewall rules are in [docs/SECURITY.md](docs/SECURITY.md).

To report a vulnerability, please contact the maintainers privately
rather than opening a public issue.

## Development

Requirements: the Rust toolchain pinned in `rust-toolchain.toml`,
Docker (for the throwaway test Postgres and the end-to-end tests),
Node with pnpm (for the UI).

```sh
# the gate, before every commit
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
bash scripts/test.sh        # starts a throwaway Postgres container on port 55440

# the web UI (React, embedded into the binary at build time)
pnpm -C ui install
pnpm -C ui lint && pnpm -C ui typecheck && pnpm -C ui test
bash scripts/build-ui.sh    # production bundle for `cargo build`

# end-to-end proofs (see docs/E2E.md)
SETU_E2E_DIR=/tmp/setu-e2e bash scripts/e2e.sh                  # 14 assertions in a systemd container
SETU_INSTALL_DIR=/tmp/setu-install bash scripts/test-install.sh  # the installer, 9 assertions

bash scripts/check-links.sh # relative links in the docs
```

`scripts/test.sh` refuses port 5432; tests never touch a real
database. Migrations are forward-only; after changing a query,
regenerate `.sqlx/` with `scripts/sqlx-prepare.sh`. UI API types come
from the checked-in `openapi.json` (`pnpm -C ui gen:api`).

## Documentation

| Document | What it covers |
|---|---|
| [docs/INSTALL.md](docs/INSTALL.md) | `install.sh` and its flags, the setup wizard, manual install, upgrading, uninstalling |
| [docs/OPERATIONS.md](docs/OPERATIONS.md) | scheduler and backups, control-plane backup and restore, upgrades, TLS mode, token rotation, rebuilding a server |
| [docs/MANIFEST.md](docs/MANIFEST.md) | the app manifest: fields, env rules, health, examples |
| [docs/AGENT.md](docs/AGENT.md) | `setu agent install`, `doctor`, state and reconcile, the agent protocol |
| [docs/SECURITY.md](docs/SECURITY.md) | trust model, the privileged helper, network, secrets at rest |
| [docs/E2E.md](docs/E2E.md) | the end-to-end and installer tests |
| [docs/RELEASING.md](docs/RELEASING.md) | cutting a release |
| [docs/ROADMAP.md](docs/ROADMAP.md) | phases 2 to 4 (containers, billing, catalogue) and later |
| [docs/specs/2026-10-09-setu-design.md](docs/specs/2026-10-09-setu-design.md) | the design spec, as built |
| [docs/brand/README.md](docs/brand/README.md) | logo, mark and colours |
| [CHANGELOG.md](CHANGELOG.md) | release notes |

## Status and contributing

Setu 0.1.3 supports Ubuntu 22.04, 24.04 and 26.04 and Debian 12 and 13, on
x86_64 and aarch64 (see the support matrix in `docs/INSTALL.md`); other
distributions are refused by the installer unless `--force-distro` is
given. Expect rough edges and breaking changes before
1.0.

Bug reports, questions and app manifests are welcome as
[GitHub issues](https://github.com/setucp/setu/issues). For anything
larger than a fix, please open an issue to discuss it before sending a
pull request, and run the gate above before you do.

## Licence

[AGPL-3.0](LICENSE).
