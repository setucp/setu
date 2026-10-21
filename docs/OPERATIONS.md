# Operations

Day-two tasks for a Setu install. Commands run as root unless shown
otherwise. The control plane reads `setu.toml` from its working
directory, so its own commands run as `setu` from `/etc/setu`.

## What runs where

| Unit | Box | Does |
|---|---|---|
| `setu.service` | control plane | `setu serve`: API, UI, job runner, scheduler; socket `/run/setu/http.sock` |
| `setu-agent.service` | every server | `setu agent run` |
| `caddy-api.service` | every server | Caddy with `/etc/caddy/setu.json`, admin socket `/run/caddy/admin.sock` |
| `setu-nftables.service` | every server | loads `table inet setu` |
| `postgresql.service` | every server | site databases; the control plane's `setu` database on its box |
| `setu-site@<short>.service` | every server | one per site |

Logs: `journalctl -u setu`, `journalctl -u setu-agent`, `journalctl -u
'setu-site@<short>'` (also the site's Logs tab), `journalctl -u
caddy-api`. Health: `setu agent doctor` on a server; `/healthz` and
`/readyz` on the panel.

## The scheduler

`setu serve` runs a scheduler that ticks every minute and:

- enqueues one `backup_site` job (kind `scheduled`) per **running** site
  each night at 03:00 in the control plane's local time zone, offset
  per site by up to ±20 minutes (a hash of the site id) so a server's
  sites do not all dump at once. A site stays due for two hours after
  its time (a control-plane restart around 03:00 does not skip a
  night), and is skipped when its newest scheduled backup is less than
  12 hours older than that day's due time or it already has an active
  job;
- enqueues `delete_backup` jobs for scheduled backups beyond the newest
  7 per site, and for `pre_update`, `pre_restore` and `pre_delete`
  backups older than 30 days. Manual backups are never deleted
  automatically;
- prunes site metrics older than 30 days, finished agent commands older
  than 24 hours (redacting any secrets first) and expired sessions.

If the control plane is down from 03:00 until after the two-hour
window, that night's backups are skipped; the next night catches up.

The control plane's own database has a separate nightly dump, run beside
the scheduler; see *Backing up the control plane*.

## Site backups

Location, on the server that runs the site:
`/var/lib/setu/backups/<short>/<backup_id>/` with `db.dump` (`pg_dump
-Fc` of `site_<short>`) and `dirs.tar.zst` (the manifest's `[backup]
dirs`), owned by `setu`, mode 0750, **not encrypted** and **not copied
off the box** in phase 1. Copy them elsewhere yourself, for example a
nightly `rsync -a /var/lib/setu/backups/ backup-host:setu/<server>/`
as root.

| Kind | Made by | Kept |
|---|---|---|
| `scheduled` | the nightly scheduler | newest 7 per site |
| `manual` | **Backups → Back up now** | until deleted |
| `pre_update` | every update, before the new version is installed | 30 days |
| `pre_restore` | every restore, before it replaces anything | 30 days |
| `pre_delete` | every site delete | 30 days (outlives the site) |

**Restore** (site page → Backups → Restore) takes a `pre_restore`
backup, stops the site, replaces its database (drop, recreate,
`pg_restore`) and swaps its backed-up dirs, starts it and waits for
health. Only backups of the same, still existing site can be restored
from the UI. The backups of a deleted site stay listed under
**Backups** (`GET /api/v1/backups`, `site_id` null) and can be deleted
there; restoring one into a new site is manual:

```sh
# on the server, after provisioning a new site of the same app (short id NEW)
systemctl stop setu-site@NEW
sudo -u postgres pg_restore --clean --if-exists --no-owner --role=site_NEW -d site_NEW \
  < /var/lib/setu/backups/OLD/<backup_id>/db.dump
# dirs: unpack into /srv/setu/sites/NEW/data/ and give them to the site user
zstd -dc /var/lib/setu/backups/OLD/<backup_id>/dirs.tar.zst | tar -x -C /srv/setu/sites/NEW/data
chown -R setu-site-NEW:setu-site-NEW /srv/setu/sites/NEW/data
systemctl start setu-site@NEW
```

The new site has its own generated secrets (for Vyasa,
`VYASA_SECRET_KEY`); data the app encrypted under the old site's key
does not open with the new one, and the deleted site's env is gone with
its row.

## Backing up the control plane

The control plane's state is its Postgres database plus
`/etc/setu/setu.toml`. Both are needed: every site secret in the
database is sealed with `secret_key` from `setu.toml`, and without that
key they cannot be opened.

```sh
sudo -u setu setu backup --out /var/lib/setu/setu-$(date +%F).dump
```

`setu` reads `setu.toml` from the current directory, else
`/etc/setu/setu.toml` when the user running it can read it (root or
`setu`; since 0.1.5, so no `cd /etc/setu` is needed).

`setu backup --out <file>` runs `pg_dump -Fc` against the configured
`database_url`, passing the password in `pg_dump`'s environment, never
on its command line. Copy the dump **and** a copy of `setu.toml` (it
holds the database password and the secret key: store it like a
private key) off the box.

`setu serve` also dumps the database itself every night at 03:00 (panel
time; if the panel was down then, as soon as it is back) into
`/var/lib/setu/backups/setu/setu-<UTC time>.dump`, keeping the newest
seven. Settings → Panel shows the last successful one; a failed attempt
is shown there too and retried hourly. These copies live on the same box:
still copy them, and `setu.toml`, off it.

## Restoring the control plane

On the same box, or a fresh one after `install.sh` with the same
`--panel-host`:

```sh
systemctl stop setu
install -o setu -g setu -m 0600 /path/to/saved/setu.toml /etc/setu/setu.toml
sudo -u postgres psql -X -c 'DROP DATABASE IF EXISTS setu WITH (FORCE)' \
  -c 'CREATE DATABASE setu OWNER setu' \
  -c 'REVOKE CONNECT ON DATABASE setu FROM PUBLIC' \
  -c 'GRANT CONNECT ON DATABASE setu TO setu'
sudo -u postgres pg_restore --no-owner --role=setu -d setu < /path/to/setu.dump
systemctl start setu
```

On a fresh box the saved `setu.toml` carries the old database
password: either set the role's password to it (`sudo -u postgres psql
-X` with `\set pw '<password>'` then `ALTER ROLE setu PASSWORD :'pw';`
typed on stdin, so it stays off the command line) or put the new
password into the restored file. Agents keep working: their tokens are
in the restored database. Jobs that were running resume from their
stored step once the lease expires.

## Upgrading

On each server, as root:

```sh
setu upgrade                    # the latest release
setu upgrade --version 0.1.5    # a given release
```

`setu upgrade` (0.1.5) downloads the release archive for this machine
and `SHA256SUMS`, refuses a checksum mismatch, and checks the new binary
reports the version asked for. On the control-plane box
(`setu.service` enabled) it then takes a control-plane backup into
`/var/lib/setu/backups/setu/` (the nightly dump's directory and
rotation) and stops if that fails. It installs `/opt/setu/setu`,
restarts `setu.service` (migrations run on boot) and, when enabled,
`setu-agent.service`, and on the control plane waits up to 90 s for
`/healthz`. Upgrade the control plane first, then the other servers.
Older releases are refused (migrations are forward-only) unless
`--allow-downgrade`.

Re-running the installer with the same flags and `--version <new>` still
works and does the same install and restarts, without the backup.

Site apps are updated separately, per site, from the UI. An app
published from a URL follows it: Setu fetches the URL every 6 hours and
publishes any new version, which then appears in each site's Updates
tab. **Check now** on the Apps page checks at once. Sites are never
updated on their own, and a version already published is never
replaced by a check (publish by hand to replace it).

## Changing the TLS mode or the panel host

Run `install-system` again with the new settings; it rewrites
`/etc/caddy/setu.json` and applies it through the admin socket, keeping
every site route:

```sh
# switch this server to ACME certificates (drop --tls internal)
setu agent install-system --panel-host setu.example.com --upstream unix//run/setu/http.sock
# or to Caddy's internal CA
setu agent install-system --tls internal --panel-host setu.example.com --upstream unix//run/setu/http.sock
```

Leave out `--panel-host`/`--upstream` on servers that do not host the
panel. On the control-plane box, also record the mode so the wizard's
install command matches (`cd /etc/setu && sudo -u setu setu setting set
tls_internal true|false`). Re-running `setu agent install` is not the
way to do this: it always joins again and needs a fresh join token.
Changing the panel's name also means editing `public_url` in
`setu.toml` (and `systemctl restart setu`), and `panel.host` in
`/etc/setu/agent.toml` (and `systemctl restart setu-agent`) so
reconcile keeps the new route.

## Rotating a server token

Servers → the server → **Rotate token** (owner only) issues a single-use
join token, valid for one hour, and shows it once. The server's status
reads `rejoining` until its agent joins with the new token. The current
server token keeps working until then, so the agent is never cut off by
the rotation itself. On the server, as root:

```sh
setu agent join        # paste the token when asked, or: SETU_JOIN_TOKEN=... setu agent join
```

`setu agent join` reads the control plane from `/etc/setu/agent.toml`,
keeps every other field of it (paths, intervals, the panel route) and
keeps its CA (`ca`) and `insecure` setting unless a flag adds to them;
under `tls internal` the printed command already carries `--ca` for
Caddy's local CA. It restarts the running agent so it uses the new
server token (#8). A lost join response is recovered by running the same
join again with the same token; rotating a second time before the join
replaces the first join token. After the join, the old server token is
invalid.

The procedure replaces the earlier manual SQL update, which could leave
the agent with a token the control plane no longer accepted.

## Rebuilding a server

Phase 1 has no site move, and backups live on the server itself, so a
server that is lost is recovered only from backups you copied off it.
The procedure below reuses the server's row (its sites keep their ids,
ports and env) and is not covered by the automated tests:

1. Install Ubuntu 24.04 again and copy `/opt/setu/setu` (root:root
   0755) onto it.
2. Restore `/var/lib/setu/backups/` from your copy (`setu:setu`,
   directories 0750).
3. Run `setu agent install --control-plane <url> [--tls internal]` with
   a join token from a **new** pending server (Servers → Add server),
   then stop the agent and point it at the old row: put the old
   server's id in `server_id` in `/etc/setu/agent.toml`, rotate the
   old server's token in the UI and run `setu agent join` as above.
   Delete the new, now unused pending server in the UI.
4. For each site on the old server: mark it `failed` (`UPDATE sites SET
   status = 'failed', failed_step = 'validate' WHERE id = '<site_id>'`
   in the `setu` database), then **Retry** in the UI. Provisioning
   re-runs from the first step with the same allocations and secrets.
5. Restore each site's newest backup from its Backups tab.

## Adding servers, cordoning, removing

**Servers → Add server** shows an install command and, separately, a
one-time join token (1 hour). Cordon a server to stop new sites being
placed on it. A server can be deleted only when it has no sites.
