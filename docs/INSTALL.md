# Installing Setu

Requirements: one Ubuntu 24.04 (x86_64 or aarch64) machine with a
public IP, root access, and ports 80/443 reachable. The installer
brings its own PostgreSQL and Caddy (via the agent step).

## The script

As root:

```sh
curl -fsSL https://github.com/vyasa-cms/setu/releases/latest/download/install.sh | sh
```

Options: `--version X.Y.Z` pins a version (default: latest release),
`--public-url https://setu.example.com` sets the panel's public URL
(default: `http://<first non-loopback IP>:7000`).
`SETU_RELEASE_BASE_URL` overrides the download root (used by
`scripts/test-install.sh` to install from a local file server).

What it does, in order:

1. Detects the arch and downloads `setu-<ver>-<target>.tar.gz` plus
   `.sha256` from GitHub releases; verifies the checksum.
2. Installs the single `setu` binary to `/opt/setu/setu`.
3. Creates the `setu` system user and `/var/lib/setu`, `/srv/setu`.
4. Installs PostgreSQL via apt when missing; creates the `setu` role
   (generated password) and the `setu` database.
5. Writes `/etc/setu/setu.toml` (mode 0640, `root:setu`) with a
   generated 32-byte secret key, the database URL and the public URL.
6. Writes and enables `setu.service` (`setu serve` behind
   `setu migrate` as `ExecStartPre`), starts it, waits for `/healthz`.
7. Prints the setup URL: `http://<ip>:7000/setup`.

Then open the setup URL: create the owner (works exactly once), note
the panel URL, register the server, and run the shown
`setu agent install …` line as root. That is the whole single-node
install.

## Manual install

1. Fetch `setu-<ver>-<target>.tar.gz` + `.sha256` from the release,
   verify, unpack the `setu` binary to `/opt/setu/setu`.
2. Provide a Postgres 16+ database and three env vars (or a
   `setu.toml` in the working directory):
   `SETU_DATABASE_URL`, `SETU_SECRET_KEY` (64 hex chars),
   `SETU_PUBLIC_URL`. Optional: `SETU_BIND_ADDR` (default
   `127.0.0.1:7000`), `SETU_DATA_DIR` (default `/srv/setu`).
3. `setu migrate`, then `setu serve` (systemd unit in
   E2E.md style: `User=setu`,
   `WorkingDirectory=/etc/setu`, `ExecStart=/opt/setu/setu serve`).
4. Open `http://<host>:7000/setup`.

## Upgrading

Download the new release archive, verify the checksum, replace
`/opt/setu/setu`, `systemctl restart setu` (migrations run on boot
under an advisory lock). Site updates are separate: each site moves
to a newer app version from the UI with automatic rollback.

## Uninstalling

`systemctl disable --now setu`; optionally remove `/opt/setu`,
`/etc/setu`, `/var/lib/setu`, `/srv/setu`, the `setu` Postgres role
and database, and the `setu` user.
