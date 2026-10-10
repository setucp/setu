# Installing Setu

Requirements: one Ubuntu 24.04 x86_64 machine (other Ubuntu releases
get a warning, other distributions are refused), root, and ports 80
and 443 reachable. For a real certificate, a DNS name pointing at the
machine. The installer brings PostgreSQL 16 and Caddy itself.

## The script

The repository and its releases are private until the owner decides
otherwise (spec §12); with access, as root:

```sh
curl -fsSLO https://github.com/vyasa-cms/setu/releases/latest/download/install.sh
bash install.sh --panel-host setu.example.com
```

Options:

| Flag | Meaning |
|---|---|
| `--panel-host <dns>` | the panel's name; Caddy gets a certificate for it (ACME) |
| `--tls internal` | Caddy issues from its own internal CA instead of ACME; alone, the panel host is the machine's primary IP |
| `--version <v>` | release to install (default: the latest) |
| `--archive-url <url>`, `--sums-url <url>` | install from these URLs instead of the GitHub release (tests; both or neither) |

One of `--panel-host` and `--tls internal` is required; both together
give an internal-CA certificate for a name (what
`scripts/test-install.sh` does with `setu.test`).

What it does, in order (spec §8):

1. Checks root, x86_64 Linux, Ubuntu 24.04.
2. `apt-get install postgresql-16 sudo gnupg zstd nftables` (and curl).
3. Downloads `setu-<ver>-x86_64-unknown-linux-musl.tar.gz` and
   `SHA256SUMS` from the release, verifies the archive, installs
   `/opt/setu/setu` (root:root 0755) and links `/usr/local/bin/setu`.
4. Creates the system user `setu`, `/var/lib/setu` (setu 0750) and
   `/etc/setu` (root 0755).
5. Creates the `setu` role (a generated password, handed to `psql` on
   stdin with `\set`, never on a command line) and database,
   `REVOKE CONNECT ON DATABASE setu FROM PUBLIC` and `GRANT CONNECT …
   TO setu`.
6. Writes `/etc/setu/setu.toml` (setu:setu 0600):
   `listen = "unix:/run/setu/http.sock"`, `database_url`, a generated
   `secret_key` (64 hex), `public_url = "https://<panel host>"`,
   `trusted_proxies = ["127.0.0.1/32"]`, `data_dir = "/var/lib/setu"`.
7. `setu agent install-system --caddy-only --panel-host <host>
   --upstream unix//run/setu/http.sock [--tls internal]`: installs the
   `caddy` package when missing (Caddy's apt repository, else Caddy's
   checksum-pinned release `.deb`), writes `/etc/caddy/setu.json` with
   the panel route, runs `caddy-api.service` on its admin socket.
8. Puts `setu` in group `caddy` (the socket is handed to that group)
   and writes `setu.service` (`User=setu`, `WorkingDirectory=/etc/setu`,
   `RuntimeDirectory=setu` 0755 holding the socket 0660 `setu:caddy`, `Restart=always`).
9. `setu migrate`, `setu setting set tls_internal true|false`, starts
   the service and waits for `https://<host>/healthz` through Caddy.
10. Prints the setup URL and the one-time setup token from
    `/var/lib/setu/setup-token` (written by `setu serve` on its first
    boot, 0600, removed once the owner exists).

Re-running the script is safe: an existing `setu.toml` is kept (its
database password and secret key are reused; a file it cannot read back,
or one whose `public_url` differs from the flags, stops the script with
an error rather than being rewritten); the service restarts only when
the binary or the unit changed.

## The setup wizard

Open `https://<host>/setup`, paste the setup token, create the owner
(works exactly once), then let the wizard register this machine: it
shows a `setu agent install --control-plane https://<host> --panel-host
<host> [--tls internal --ca …]` line and a separate join token. Run the
line as root with the token in `SETU_JOIN_TOKEN` (or paste it on stdin
when asked):

```sh
SETU_JOIN_TOKEN=<token> setu agent install --control-plane https://setu.example.com --panel-host setu.example.com
```

The agent joins through Caddy (the panel has no TCP listener), and
the server shows `online` within seconds. With `--tls internal`, browsers
need Caddy's root certificate,
`/var/lib/caddy/.local/share/caddy/pki/authorities/local/root.crt`.

## Commands on the host

The control plane reads `setu.toml` from its working directory, so run
its commands as `setu` from `/etc/setu`:

```sh
cd /etc/setu
sudo -u setu setu backup --out /var/lib/setu/setu-$(date +%F).dump   # pg_dump -Fc
SETU_OPERATOR_PASSWORD=… sudo -E -u setu setu operator create --email a@b.c --role operator
sudo -u setu setu setting set tls_internal true
```

`setu backup` hands the database password to `pg_dump` in its
environment (`PGPASSWORD`), never on argv; restoring it, and keeping
`setu.toml` (the key to every sealed secret) with it, is in
[OPERATIONS.md](OPERATIONS.md#backing-up-the-control-plane).
`operator create` takes the password from `SETU_OPERATOR_PASSWORD` or
the first line of stdin.

## Manual install

1. Fetch the archive and `SHA256SUMS` from the release, `sha256sum -c
   --ignore-missing SHA256SUMS`, unpack `setu` to `/opt/setu/setu`.
2. Provide a Postgres 16 database and either a `setu.toml` in the
   service's working directory or `SETU_*` variables (prefix `SETU_`,
   nested keys `__`): `SETU_DATABASE_URL`, `SETU_SECRET_KEY` (64 hex),
   `SETU_PUBLIC_URL`; optional `SETU_LISTEN` (`<ip>:<port>`, default
   `127.0.0.1:7000`, or `unix:<path>`: created 0660 and handed to group
   `caddy`), `SETU_TRUSTED_PROXIES` (comma-separated, default
   `127.0.0.1/32`), `SETU_DATA_DIR` (default `/var/lib/setu`).
3. `setu migrate`, then `setu serve` under systemd, behind Caddy.

## Upgrading

Re-run `install.sh --version <new> …` with the same flags: it verifies
the new archive, replaces the binary and restarts `setu.service`
(migrations run on boot) and, when the local agent is installed,
`setu-agent.service` (only when the binary changed; R139). Upgrade other
servers' binaries by hand ([OPERATIONS.md](OPERATIONS.md#upgrading)). Back up
first (`setu backup`); migrations are forward-only. Site updates are
separate: each site moves to a newer app version from the UI with
automatic rollback.

## Uninstalling

`systemctl disable --now setu setu-agent`; then remove `/opt/setu`,
`/usr/local/bin/setu`, `/etc/setu`, `/var/lib/setu`, `/srv/setu`, the
`setu` role and database, and the `setu` user as needed.
