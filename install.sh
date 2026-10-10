#!/usr/bin/env bash
# Setu installer (spec §8, 0.1.2 §1-2). Run as root on a fresh server
# (Ubuntu 22.04/24.04 or Debian 12/13; x86_64 or aarch64):
#
#   curl -fsSLO https://github.com/setucp/setu/releases/latest/download/install.sh
#   sudo bash install.sh --panel-host setu.example.com
#
# It downloads the release archive and SHA256SUMS, verifies the archive,
# installs /opt/setu/setu (root 0755), creates the `setu` user, installs
# PostgreSQL 16, creates the `setu` database and role (generated password,
# `REVOKE CONNECT … FROM PUBLIC`), writes /etc/setu/setu.toml (0600), lets
# `setu agent install-system --caddy-only` install and configure Caddy
# (the panel route to /run/setu/http.sock), writes and starts
# setu.service, waits for /healthz through Caddy, and prints the setup URL
# with the one-time setup token. Re-running it keeps the existing
# setu.toml (database password and secret key) and changes nothing else
# that is already in place.
#
# Options:
#   --force-distro        on another distribution, use the Debian 12 plan
#                         (printed) instead of refusing
#   --panel-host <dns>    the panel's DNS name (Caddy obtains a certificate
#                         for it); required unless --tls internal is given
#   --tls internal        Caddy issues from its internal CA instead of ACME;
#                         without --panel-host the panel host is this
#                         machine's primary IP address
#   --version <v>         release to install (default: the latest)
#   --archive-url <url>   take the archive from <url> (tests; needs --sums-url)
#   --sums-url <url>      take SHA256SUMS from <url> (tests; needs --archive-url)
set -euo pipefail

REPO="setucp/setu"
CONF=/etc/setu/setu.toml
UNIT=/etc/systemd/system/setu.service
SOCKET=/run/setu/http.sock
TOKEN_FILE=/var/lib/setu/setup-token
CADDY_CA=/var/lib/caddy/.local/share/caddy/pki/authorities/local/root.crt
# The PGDG repository (spec 0.1.2 §2): key pinned by SHA-256, the same
# value as crates/agent/src/distro.rs (a shell test checks that).
PGDG_KEY_URL=https://www.postgresql.org/media/keys/ACCC4CF8.asc
PGDG_KEY_SHA256=0144068502a1eddd2a0280ede10ef607d1ec592ce819940991203941564e8e76
PGDG_KEYRING=/etc/apt/keyrings/apt.postgresql.org.asc
PGDG_LIST=/etc/apt/sources.list.d/pgdg.list

PANEL_HOST=""
TLS=""
VERSION=""
ARCHIVE_URL=""
SUMS_URL=""
FORCE_DISTRO=0
PLAN=""

say() { printf '%s\n' "$*"; }
warn() { printf 'install.sh: warning: %s\n' "$*" >&2; }
fail() { printf 'install.sh: %s\n' "$*" >&2; exit 1; }
usage() { sed -n '2,/^set -euo/p' "$0" | sed -e '/^set -euo/d' -e 's/^# \{0,1\}//'; }

# select_archive <uname -m>: the release target triple, or status 2.
select_archive() {
    case "$1" in
        x86_64) echo x86_64-unknown-linux-musl ;;
        aarch64 | arm64) echo aarch64-unknown-linux-musl ;;
        *) return 2 ;;
    esac
}

# detect_distro [os-release]: prints "<id> <version> <plan>", where plan is
# ubuntu-24.04, ubuntu-22.04, debian-12, debian-13 or unsupported. The
# table mirrors crates/agent/src/distro.rs (spec 0.1.2 §2).
detect_distro() {
    local file="${1:-/etc/os-release}" id version plan
    if [[ ! -r "$file" ]]; then
        echo "unknown unknown unsupported"
        return 0
    fi
    id="$(. "$file" && echo "${ID:-unknown}")"
    version="$(. "$file" && echo "${VERSION_ID:-unknown}")"
    case "$id $version" in
        "ubuntu 24.04") plan=ubuntu-24.04 ;;
        "ubuntu 22.04") plan=ubuntu-22.04 ;;
        "debian 12") plan=debian-12 ;;
        "debian 13") plan=debian-13 ;;
        *) plan=unsupported ;;
    esac
    echo "$id $version $plan"
}

# resolve_plan <id> <version> <detected plan> <force 0|1>: sets PLAN. An
# unsupported distribution is refused (status 2) unless forced; then the
# Debian 12 plan is used and the choice is printed.
resolve_plan() {
    local id=$1 version=$2 detected=$3 force=$4
    if [[ "$detected" != unsupported ]]; then
        PLAN=$detected
        return 0
    fi
    if [[ "$force" != 1 ]]; then
        printf 'install.sh: unsupported distribution %s %s; Setu supports Ubuntu 22.04/24.04 and Debian 12/13 (pass --force-distro to use the Debian 12 plan)\n' \
            "$id" "$version" >&2
        return 2
    fi
    PLAN=debian-12
    say "using the Debian 12 plan for ${id} ${version} (--force-distro)"
}

# pgdg_codename <plan>: the PGDG codename the plan needs; status 1 when the
# distribution ships PostgreSQL 16 or 17 itself.
pgdg_codename() {
    case "$1" in
        ubuntu-22.04) echo jammy ;;
        debian-12) echo bookworm ;;
        *) return 1 ;;
    esac
}

# postgres_major <plan>: 17 on Debian 13, 16 everywhere else.
postgres_major() {
    case "$1" in
        debian-13) echo 17 ;;
        *) echo 16 ;;
    esac
}

# pgdg_list_line <codename>: the apt source, bound to the pinned key.
pgdg_list_line() {
    printf 'deb [signed-by=%s] https://apt.postgresql.org/pub/repos/apt %s-pgdg main\n' \
        "$PGDG_KEYRING" "$1"
}

# verify_pgdg_key <file>: status 1 unless the file is the pinned key. Runs
# before apt sees the repository.
verify_pgdg_key() {
    local actual
    actual="$(sha256sum "$1" | awk '{print $1}')"
    if [[ "$actual" != "$PGDG_KEY_SHA256" ]]; then
        printf 'install.sh: PGDG keyring checksum mismatch: got %s, expected %s\n' \
            "$actual" "$PGDG_KEY_SHA256" >&2
        return 1
    fi
}

# Sourced by the shell unit test: define the functions, run nothing.
if [[ -n "${SETU_INSTALL_SOURCE_ONLY:-}" ]]; then
    return 0
fi

while [[ $# -gt 0 ]]; do
    case "$1" in
        --force-distro) FORCE_DISTRO=1; shift ;;
        --panel-host) PANEL_HOST="${2:?--panel-host needs a value}"; shift 2 ;;
        --tls) TLS="${2:?--tls needs a value}"; shift 2 ;;
        --version) VERSION="${2:?--version needs a value}"; shift 2 ;;
        --archive-url) ARCHIVE_URL="${2:?--archive-url needs a value}"; shift 2 ;;
        --sums-url) SUMS_URL="${2:?--sums-url needs a value}"; shift 2 ;;
        -h | --help) usage; exit 0 ;;
        *) fail "unknown argument $1 (see --help)" ;;
    esac
done

[[ -z "$TLS" || "$TLS" == internal ]] || fail "--tls takes only 'internal'"
[[ -n "$PANEL_HOST" || "$TLS" == internal ]] || fail "pass --panel-host <dns> or --tls internal"
[[ -z "$ARCHIVE_URL" && -z "$SUMS_URL" ]] || [[ -n "$ARCHIVE_URL" && -n "$SUMS_URL" ]] ||
    fail "--archive-url and --sums-url go together"
VERSION="${VERSION#v}"

# ---------------------------------------------------------------- checks

[[ "$(id -u)" == 0 ]] || fail "run as root"
[[ "$(uname -s)" == Linux ]] || fail "Linux only"
TARGET="$(select_archive "$(uname -m)")" ||
    fail "unsupported machine type $(uname -m): Setu ships x86_64 and aarch64 archives"
read -r os_id os_version detected_plan < <(detect_distro)
resolve_plan "$os_id" "$os_version" "$detected_plan" "$FORCE_DISTRO" || exit 2

if [[ -z "$PANEL_HOST" ]]; then
    PANEL_HOST="$(hostname -I 2>/dev/null | awk '{print $1}')"
    [[ -n "$PANEL_HOST" ]] || fail "cannot find this machine's primary IP; pass --panel-host"
    say "Panel host: ${PANEL_HOST} (primary IP, --tls internal)"
fi
[[ "$PANEL_HOST" =~ ^[A-Za-z0-9]([A-Za-z0-9.-]{0,251}[A-Za-z0-9])?$ ]] ||
    fail "--panel-host must be a DNS name or IPv4 address"
PUBLIC_URL="https://${PANEL_HOST}"

# ---------------------------------------------------------------- packages

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

export DEBIAN_FRONTEND=noninteractive
apt-get update -q
# sudo first: Debian 12 images may lack it, and the postgres steps use it.
apt-get install -y -q sudo gnupg ca-certificates curl
PG_MAJOR="$(postgres_major "$PLAN")"
if pg_codename="$(pgdg_codename "$PLAN")"; then
    say "Adding the PostgreSQL apt repository (${pg_codename}-pgdg)"
    curl -fsSL --retry 3 --proto '=https' -o "${tmp}/pgdg.asc" "$PGDG_KEY_URL" ||
        fail "download failed: $PGDG_KEY_URL"
    # A wrong key stops here, before apt has a repository to read.
    verify_pgdg_key "${tmp}/pgdg.asc" ||
        fail "the PostgreSQL key does not match the pinned checksum; apt was not changed"
    install -d -m 0755 /etc/apt/keyrings
    install -m 0644 "${tmp}/pgdg.asc" "$PGDG_KEYRING"
    pgdg_list_line "$pg_codename" >"${tmp}/pgdg.list"
    install -m 0644 "${tmp}/pgdg.list" "$PGDG_LIST"
    apt-get update -q
fi
say "Installing packages (postgresql-${PG_MAJOR}, zstd, nftables)"
apt-get install -y -q "postgresql-${PG_MAJOR}" zstd nftables

# ---------------------------------------------------------------- download

if [[ -z "$ARCHIVE_URL" ]]; then
    if [[ -z "$VERSION" ]]; then
        VERSION="$(curl -fsSL --proto '=https' "https://api.github.com/repos/${REPO}/releases/latest" |
            sed -n 's/.*"tag_name": *"v\{0,1\}\([^"]*\)".*/\1/p' | head -n 1)"
        [[ -n "$VERSION" ]] || fail "cannot find the latest release; pass --version"
    fi
    base="https://github.com/${REPO}/releases/download/v${VERSION}"
    ARCHIVE_URL="${base}/setu-${VERSION}-${TARGET}.tar.gz"
    SUMS_URL="${base}/SHA256SUMS"
fi
archive="$(basename "${ARCHIVE_URL%%\?*}")"
[[ "$archive" =~ ^setu-[0-9A-Za-z.+-]+-${TARGET}\.tar\.gz$ ]] || fail "unexpected archive name $archive"
say "Downloading ${archive}"
curl -fsSL --retry 3 -o "${tmp}/${archive}" "$ARCHIVE_URL" || fail "download failed: $ARCHIVE_URL"
curl -fsSL --retry 3 -o "${tmp}/SHA256SUMS" "$SUMS_URL" || fail "download failed: $SUMS_URL"
expected="$(awk -v f="$archive" '$2 == f || $2 == "*" f { print $1 }' "${tmp}/SHA256SUMS")"
[[ "$expected" =~ ^[0-9a-f]{64}$ ]] || fail "SHA256SUMS has no entry for $archive"
actual="$(sha256sum "${tmp}/${archive}" | awk '{print $1}')"
[[ "$expected" == "$actual" ]] || fail "checksum mismatch for $archive"
say "Checksum verified"
mkdir "${tmp}/x"
tar -xzf "${tmp}/${archive}" -C "${tmp}/x"
[[ -f "${tmp}/x/setu" ]] || fail "the archive has no top-level setu binary"

BIN_CHANGED=0
if ! cmp -s "${tmp}/x/setu" /opt/setu/setu; then
    install -d -o root -g root -m 0755 /opt/setu
    install -o root -g root -m 0755 "${tmp}/x/setu" /opt/setu/.setu.new
    mv -f /opt/setu/.setu.new /opt/setu/setu
    BIN_CHANGED=1
fi
ln -sfn /opt/setu/setu /usr/local/bin/setu
say "Installed $(/opt/setu/setu --version)"

# ---------------------------------------------------------------- user, dirs

id setu >/dev/null 2>&1 ||
    useradd -r -d /var/lib/setu -s /usr/sbin/nologin setu
install -d -o setu -g setu -m 0750 /var/lib/setu
install -d -o root -g root -m 0755 /etc/setu

# ---------------------------------------------------------------- config

# Re-runs keep the file: the secret key seals every site secret and the
# password is the role's. Anything we cannot read back is an error, never
# a silent rewrite.
KEEP_CONF=0
if [[ -e "$CONF" ]]; then
    DB_PASSWORD="$(sed -n 's|^database_url = "postgres://setu:\([A-Za-z0-9]\{16,\}\)@127\.0\.0\.1:5432/setu"$|\1|p' "$CONF")"
    secret_ok="$(grep -c '^secret_key = "[0-9a-f]\{64\}"$' "$CONF" || true)"
    old_url="$(sed -n 's|^public_url = "\(.*\)"$|\1|p' "$CONF")"
    if [[ -z "$DB_PASSWORD" || "$DB_PASSWORD" == *$'\n'* || "$secret_ok" != 1 || -z "$old_url" ]]; then
        fail "cannot parse ${CONF} (database_url, secret_key, public_url as install.sh writes them); fix it or move it aside"
    fi
    [[ "$old_url" == "$PUBLIC_URL" ]] ||
        fail "${CONF} has public_url ${old_url}, not ${PUBLIC_URL}; edit it first or pass the same host"
    KEEP_CONF=1
    say "Keeping ${CONF}"
else
    DB_PASSWORD="$(od -An -N24 -tx1 /dev/urandom | tr -d ' \n')"
fi

# ---------------------------------------------------------------- postgres

systemctl enable --now postgresql >/dev/null 2>&1 || systemctl start postgresql
for _ in $(seq 1 60); do
    (cd / && sudo -u postgres psql -Xqtc 'SELECT 1') >/dev/null 2>&1 && break
    sleep 1
done
(cd / && sudo -u postgres psql -Xqtc 'SELECT 1') >/dev/null 2>&1 || fail "PostgreSQL did not start"

# The password reaches psql on stdin (`\set`), never on a command line.
(cd / && sudo -u postgres psql -X -q -v ON_ERROR_STOP=1) >/dev/null <<SQL
\set pw '${DB_PASSWORD}'
SELECT format('CREATE ROLE setu LOGIN PASSWORD %L', :'pw')
  WHERE NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'setu') \gexec
SELECT format('ALTER ROLE setu LOGIN PASSWORD %L', :'pw') \gexec
SELECT 'CREATE DATABASE setu OWNER setu'
  WHERE NOT EXISTS (SELECT 1 FROM pg_database WHERE datname = 'setu') \gexec
REVOKE CONNECT ON DATABASE setu FROM PUBLIC;
GRANT CONNECT ON DATABASE setu TO setu;
SQL
say "Database setu ready"

if [[ "$KEEP_CONF" == 0 ]]; then
    secret_key="$(od -An -N32 -tx1 /dev/urandom | tr -d ' \n')"
    (
        umask 077
        cat >"${CONF}.new" <<TOML
# Setu control plane (written by install.sh). Secrets: keep this file 0600.
listen = "unix:${SOCKET}"
database_url = "postgres://setu:${DB_PASSWORD}@127.0.0.1:5432/setu"
secret_key = "${secret_key}"
public_url = "${PUBLIC_URL}"
trusted_proxies = ["127.0.0.1/32"]
data_dir = "/var/lib/setu"
TOML
    )
    unset secret_key
    chown setu:setu "${CONF}.new"
    chmod 0600 "${CONF}.new"
    mv -f "${CONF}.new" "$CONF"
    say "Wrote ${CONF}"
fi
unset DB_PASSWORD
chown setu:setu "$CONF"
chmod 0600 "$CONF"

# ---------------------------------------------------------------- caddy

tls_args=()
[[ "$TLS" == internal ]] && tls_args=(--tls internal)
say "Installing and configuring Caddy (panel route ${PANEL_HOST} -> ${SOCKET})"
/opt/setu/setu agent install-system --caddy-only --panel-host "$PANEL_HOST" \
    --upstream "unix/${SOCKET}" "${tls_args[@]}"
# The socket is handed to group caddy (setu must be a member to chgrp it).
usermod -aG caddy setu

# ---------------------------------------------------------------- service

UNIT_CHANGED=0
cat >"${tmp}/setu.service" <<'UNITFILE'
[Unit]
Description=Setu control plane
After=postgresql.service network-online.target
Wants=postgresql.service network-online.target

[Service]
Type=simple
User=setu
Group=setu
WorkingDirectory=/etc/setu
RuntimeDirectory=setu
# 0755: Caddy must traverse /run/setu (systemd re-owns the directory to
# setu:setu before every command, so a group change does not stick);
# the socket itself is 0660 setu:caddy, so only Caddy can connect.
RuntimeDirectoryMode=0755
ExecStart=/opt/setu/setu serve
Restart=always
RestartSec=2

[Install]
WantedBy=multi-user.target
UNITFILE
if ! cmp -s "${tmp}/setu.service" "$UNIT"; then
    install -o root -g root -m 0644 "${tmp}/setu.service" "$UNIT"
    systemctl daemon-reload
    UNIT_CHANGED=1
fi

# Config is read from the working directory (setu.toml in /etc/setu).
(cd /etc/setu && sudo -u setu /opt/setu/setu migrate)
tls_internal=false
[[ "$TLS" == internal ]] && tls_internal=true
(cd /etc/setu && sudo -u setu /opt/setu/setu setting set tls_internal "$tls_internal") >/dev/null

systemctl enable setu.service >/dev/null 2>&1
if [[ "$BIN_CHANGED" == 1 || "$UNIT_CHANGED" == 1 ]] || ! systemctl is-active --quiet setu.service; then
    systemctl restart setu.service
fi
# The local agent runs the same binary: an upgrade restarts it too (only
# when the binary changed and the agent was installed, R139).
if [[ "$BIN_CHANGED" == 1 ]] && systemctl is-enabled --quiet setu-agent.service 2>/dev/null; then
    say "Restarting setu-agent.service on the new binary"
    systemctl restart setu-agent.service
fi

say "Waiting for ${PUBLIC_URL}/healthz through Caddy"
healthy=0
for _ in $(seq 1 90); do
    if curl -fsSk --max-time 5 --resolve "${PANEL_HOST}:443:127.0.0.1" \
        "${PUBLIC_URL}/healthz" >/dev/null 2>&1; then
        healthy=1
        break
    fi
    sleep 1
done
if [[ "$healthy" != 1 ]]; then
    systemctl --no-pager status setu.service caddy-api.service || true
    journalctl -u setu.service -u caddy-api.service --no-pager -n 60 || true
    fail "${PUBLIC_URL}/healthz did not answer through Caddy"
fi

say ""
say "Setu is running."
say "  binary:  /opt/setu/setu (also /usr/local/bin/setu)"
say "  config:  ${CONF}"
say "  service: systemctl status setu"
if [[ "$TLS" == internal ]]; then
    say "  TLS:     Caddy's internal CA; trust ${CADDY_CA} in your browser"
fi
if [[ -s "$TOKEN_FILE" ]]; then
    say ""
    say "Open ${PUBLIC_URL}/setup and enter this one-time setup token:"
    say "  $(cat "$TOKEN_FILE")"
else
    say "Setup is complete: sign in at ${PUBLIC_URL}/"
fi
