# Setu roadmap

Phase 1 (the MVP, `docs/plans/2026-10-09-setu-mvp.md`) is planned at
task level. Later phases are scoped here; each gets its own spec and
plan when the one before it ships, because their plans must reference
the real crate interfaces, not guessed ones. Each phase is a release:
0.1 MVP, 0.2 containers and durability, 0.3 customers and billing, 0.4
catalogue, 1.0 when a paying fleet has run on it for a quarter.

## Phase 2 — 0.2 "runs anything, loses nothing"

Goal: container images are first-class, backups leave the box, sites
can move between servers, and an operator can take a server down
without losing a site.

Scope:
- **Quadlet runtime.** `QuadletRuntime` behind the `Runtime` trait:
  rootless Podman, one `.container` unit per site, same drop-in for
  limits, same journald logs. Image pulled by digest; manifest
  `run.kind = "image"` becomes provisionable. Agent install adds
  `podman` and enables `podman-system-service` for the site users.
- **S3-compatible backups.** Per-server or per-panel backup target
  (R2, S3, MinIO, Hetzner Object Storage); nightly upload, retention
  (7 daily, 4 weekly), restore from remote, integrity check (restore
  into a scratch database weekly, report). Setu's own database backed
  up the same way.
- **Site move.** Job `move_site`: pre-move backup, provision on the
  target with the same env, restore, health, flip Caddy routes on both
  servers, stop the source, delete the source after 24 h. DNS stays
  the same when both servers sit behind Cloudflare or a floating IP;
  otherwise the UI shows the new IP to set.
- **Drain and maintenance.** `servers/{id}/drain` moves every site off;
  `cordon` stops placement. Agent self-update from the control plane.
- **Disk quotas.** XFS project quotas when the data filesystem is XFS,
  `du`-based soft limits otherwise; over-quota sites get a warning and
  then writes blocked for uploads via the app's own storage settings.
- **Deletion grace.** Delete stops the site and keeps the artefacts 24 h
  (configurable), then removes them; "Undo delete" in that window.
- **Cloudflare tunnel ingress.** Alternative to Caddy's public ports:
  the agent runs `cloudflared` with a per-server tunnel token and adds
  ingress rules per site, for servers without a public IP.
- **2FA enforcement** per panel; operator API tokens with scopes.
- **Metrics and alerts.** Keep 90 days at 1-minute resolution, roll up
  to hourly after 7 days; email or webhook when a site is `failed`,
  a server is `offline` > 5 min, a backup failed, disk > 90 %.

Design questions to settle first: Podman vs. systemd-nspawn for
images; backup encryption at rest (recommend age with a per-panel key
in the sealer's key hierarchy); whether a move keeps the port.

## Phase 3 — 0.3 "sell it"

Goal: a customer signs up, picks a plan, pays, and gets a site, with
the operator only watching.

Scope:
- **Customers.** `customers`, `customer_users`, sites owned by a
  customer; customer portal at `/portal` (own sites, usage, backups,
  invoices, restart, restore, env for `prompt` variables only).
- **Plans with prices** and quotas (sites, memory, disk, bandwidth);
  plan change resizes limits via a drop-in rewrite; upgrades
  immediate, downgrades at period end.
- **Billing.** Stripe Checkout and Billing portal; webhooks drive
  `subscription_state`; grace period then suspend (stop + holding
  page) then delete after N days with a final backup kept 30 days.
  Invoices by email. Usage metering for bandwidth (from Caddy access
  logs) and disk.
- **Placement policies.** Server groups (region, tier); plans map to
  groups; placement by free memory within the group; anti-affinity for
  a customer's sites optional.
- **Email.** SMTP configured in Settings; templates for invite,
  password reset, suspension, backup failure, invoice.
- **Multi-operator teams** with per-customer scoping (support role sees
  only assigned customers).
- **Control-plane HA, minimal:** `setu serve` can run twice behind one
  Caddy with leader election on the job runner (Postgres advisory
  lock); sessions already in Postgres.

Design questions: Stripe only or also Paddle/Lemon Squeezy for tax
handling; whether the portal is a second UI build or a role in the
same SPA (recommend the same SPA, route-gated); suspension UX.

## Phase 4 — 0.4 "catalogue"

Goal: app authors publish once; any Setu installs their app in a
click.

Scope:
- **Public registry** of signed manifests (ed25519, the Vyasa
  marketplace model): index at a static URL, per-app channels
  (`stable`, `beta`), version pinning, "update available" badges
  fed from the index.
- **"Deploy to Setu" button** spec: a URL that opens the panel's new-
  site dialog with the manifest prefilled.
- **Manifest v2**: multiple processes per app (web + worker), cron
  entries, pre-start hooks (migrations), post-provision hooks, Redis
  or other sidecars declared as `needs`, health with a startup grace,
  and `run.kind = "source"` (build from a Git tag with a declared
  toolchain in a throwaway container) for authors who do not publish
  binaries.
- **Author tooling:** `setu manifest validate|sign|publish`,
  a GitHub Action that publishes on release.
- **Vyasa integration:** Vyasa's marketplace lists "Host on Setu";
  Vyasa's updater can be driven by Setu's update job (strategy
  `command`) or by the registry's new version.

Design questions: who runs the official registry and its signing key;
review policy for listings; how source builds are isolated.

## Later, unscheduled

- Postgres as a managed service across servers (one primary cluster
  with replicas; sites' databases placed independently of their
  process).
- Windows Server agent (unlikely; only if a customer pays for it).
- Kubernetes runtime behind the same `Runtime` trait, for fleets that
  already have a cluster.
- Edge caching and WAF policies per site through Cloudflare's API.
- Audit log export and SSO (OIDC) for operators.
