#!/usr/bin/env bash
# Installs Setu on a fresh Ubuntu 24.04 server (run as root):
# downloads the checked release archive, installs the single `setu`
# binary to /opt/setu, creates a local Postgres database, writes
# /etc/setu/setu.toml, and starts setu.service. Prints the setup URL.
#
#   curl -fsSL https://github.com/vyasa-cms/setu/releases/latest/download/install.sh | sh
#
# Options:
#   --version X.Y.Z     Setu version to install (default: latest release)
#   --public-url URL    public URL of this panel, e.g. https://setu.example.com
#                       (default: http://<first non-loopback IP>:7000)
#   --no-start          install everything but do not start the service
#
# Environment overrides (mostly for testing):
#   SETU_RELEASE_BASE_URL   release root, e.g. https://github.com/vyasa-cms/setu/releases/download
#                           (default; point at a local file server in tests)
#   SETU_VERSION            same as --version
#   SETU_PUBLIC_URL         same as --public-url
set -euo pipefail

REPO="vyasa-cms/setu"
RELEASE_BASE_URL="${SETU_RELEASE_BASE_URL:-https://github.com/${REPO}/releases/download}"
VERSION="${SETU_VERSION:-}"
PUBLIC_URL="${SETU_PUBLIC_URL:-}"
NO_START=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --version) VERSION="${2:?--version needs a value}"; shift 2 ;;
        --public-url) PUBLIC_URL="${2:?--public-url needs a value}"; shift 2 ;;
        --no-start) NO_START=1; shift ;;
        *) echo "install.sh: unknown argument $1 (see --version, --public-url, --no-start)" >&2; exit 1 ;;
    esac
done

say() { printf '%s\n' "$*"; }
fail() { printf 'install.sh: %s\n' "$*" >&2; exit 1; }

[[ "$(id -u)" == "0" ]] || fail "run as root"

# Fresh machines (and minimal containers) may lack even curl: install
# the essentials via apt when it exists, else fail naming the tool.
missing=()
for tool in curl tar openssl sudo hostname; do
    command -v "$tool" >/dev/null 2>&1 || missing+=("$tool")
done
if [[ "${#missing[@]}" -gt 0 ]]; then
    if command -v apt-get >/dev/null 2>&1; then
        say "Installing essentials (${missing[*]}) via apt"
        export DEBIAN_FRONTEND=noninteractive
        apt-get update -qq
        apt-get install -y -qq curl ca-certificates tar openssl sudo hostname
    else
        fail "needs ${missing[*]} (no apt-get to install them)"
    fi
fi

arch="$(uname -m)"
case "$arch" in
    x86_64) target="x86_64-unknown-linux-musl" ;;
    aarch64|arm64) target="aarch64-unknown-linux-musl" ;;
    *) fail "no build for $arch (x86_64 and aarch64 only)" ;;
esac

if [[ -z "$VERSION" ]]; then
    # Public and private repos alike answer the API; private needs a token.
    auth_args=()
    [[ -n "${GITHUB_TOKEN:-}" ]] && auth_args=(-H "Authorization: Bearer $GITHUB_TOKEN")
    VERSION="$(curl -fsSL "${auth_args[@]}" "https://api.github.com/repos/${REPO}/releases/latest" \
        | sed -n 's/.*"tag_name": *"v\{0,1\}\([^"]*\)".*/\1/p' | head -n 1)"
    [[ -n "$VERSION" ]] || fail "could not find the latest release; pass --version X.Y.Z"
fi

if [[ -z "$PUBLIC_URL" ]]; then
    ip="$(hostname -I 2>/dev/null | awk '{print $1}')"
    [[ -n "$ip" ]] || ip="127.0.0.1"
    PUBLIC_URL="http://${ip}:7000"
fi

name="setu-${VERSION}-${target}"
base="${RELEASE_BASE_URL}/v${VERSION}"

tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
say "Downloading Setu ${VERSION} for ${target}"
curl -fsSL -o "${tmp}/${name}.tar.gz" "${base}/${name}.tar.gz" \
    || fail "download failed: ${base}/${name}.tar.gz"
curl -fsSL -o "${tmp}/${name}.tar.gz.sha256" "${base}/${name}.tar.gz.sha256" \
    || fail "checksum download failed: ${base}/${name}.tar.gz.sha256"
expected="$(awk '{print $1}' "${tmp}/${name}.tar.gz.sha256")"
actual="$(sha256sum "${tmp}/${name}.tar.gz" | awk '{print $1}')"
[[ "$expected" == "$actual" ]] || fail "checksum mismatch for ${name}.tar.gz (expected $expected, got $actual)"
say "Checksum verified"

mkdir -p /opt/setu
tar -xzf "${tmp}/${name}.tar.gz" -C "$tmp"
[[ -x "${tmp}/setu" ]] || fail "archive has no setu binary at its top level"
install -m 0755 "${tmp}/setu" /opt/setu/setu

# Service user (no login): runs setu.service and owns /var/lib/setu.
if ! id setu >/dev/null 2>&1; then
    useradd --system --no-create-home --home-dir /var/lib/setu --shell /usr/sbin/nologin setu
fi
mkdir -p /var/lib/setu /etc/setu /srv/setu/sites
chown setu:setu /var/lib/setu
chmod 0755 /srv/setu

# Local Postgres: Ubuntu's `postgresql` metapackage, then a `setu` role
# with a generated password and a `setu`-owned database over TCP.
if ! command -v psql >/dev/null 2>&1; then
    say "Installing PostgreSQL (apt)"
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq
    apt-get install -y -qq postgresql
fi
# Minimal images carry a policy-rc.d that blocks the package's own
# autostart: make sure the cluster is actually running.
if ! sudo -u postgres psql -tAc "SELECT 1" >/dev/null 2>&1; then
    ver="$(ls /etc/postgresql 2>/dev/null | head -n 1)"
    if [[ -n "$ver" ]]; then
        pg_ctlcluster "$ver" main start 2>/dev/null \
            || systemctl start postgresql 2>/dev/null || true
    else
        systemctl start postgresql 2>/dev/null || true
    fi
fi
db_password="$(openssl rand -hex 24)"
secret_key="$(openssl rand -hex 32)"
if [[ -f /etc/setu/setu.toml ]]; then
    # Re-runs (e.g. to change the public URL) must NEVER rotate the
    # secret key or the database password: the former seals every site
    # secret, the latter is baked into every DATABASE_URL. Reuse both.
    old_secret="$(sed -n 's/^secret_key *= *"//p' /etc/setu/setu.toml | tr -d '"' | head -n 1)"
    old_password="$(sed -n 's|^database_url *= *"postgres://setu:\([^@]*\)@.*|\1|p' /etc/setu/setu.toml | head -n 1)"
    if [[ -n "$old_secret" && -n "$old_password" ]]; then
        secret_key="$old_secret"
        db_password="$old_password"
        say "Reusing existing secret key and database password from /etc/setu/setu.toml"
    else
        fail "refusing to overwrite /etc/setu/setu.toml with fresh secrets (unparseable existing file; move it aside first)"
    fi
fi
sudo -u postgres psql -v ON_ERROR_STOP=1 -q <<SQL
DO \$\$
BEGIN
  IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'setu') THEN
    CREATE ROLE setu LOGIN PASSWORD '${db_password}';
  ELSE
    ALTER ROLE setu LOGIN PASSWORD '${db_password}';
  END IF;
END
\$\$;
SELECT 'CREATE DATABASE setu OWNER setu' WHERE NOT EXISTS (SELECT FROM pg_database WHERE datname = 'setu')\gexec
SQL

umask 077
cat > /etc/setu/setu.toml <<TOML
# Setu control-plane config (written by install.sh; prefer env overrides).
bind_addr = "127.0.0.1:7000"
database_url = "postgres://setu:${db_password}@127.0.0.1:5432/setu"
secret_key = "${secret_key}"
public_url = "${PUBLIC_URL}"
data_dir = "/srv/setu"
TOML
chown root:setu /etc/setu/setu.toml
chmod 0640 /etc/setu/setu.toml

cat > /etc/systemd/system/setu.service <<UNIT
[Unit]
Description=Setu control plane
After=network.target postgresql.service
Wants=postgresql.service

[Service]
Type=simple
User=setu
Group=setu
WorkingDirectory=/etc/setu
ExecStart=/opt/setu/setu serve
ExecStartPre=/opt/setu/setu migrate
Restart=on-failure
RestartSec=5
NoNewPrivileges=yes
ProtectSystem=strict
ReadWritePaths=/var/lib/setu /srv/setu

[Install]
WantedBy=multi-user.target
UNIT

systemctl daemon-reload
if [[ "$NO_START" == "1" ]]; then
    systemctl enable setu.service 2>/dev/null || true
    say "Installed Setu ${VERSION} to /opt/setu (service not started: --no-start)"
else
    systemctl enable setu.service
    systemctl start setu.service
    # Fail loudly when the unit does not come up (a silent install with a
    # dead service wastes the whole debug cycle).
    if ! systemctl is-active --quiet setu.service; then
        systemctl status setu.service --no-pager || true
        journalctl -u setu.service --no-pager -n 50 || true
        fail "setu.service did not start (see above)"
    fi
    # Wait for the health probe before printing the setup URL.
    for _ in $(seq 1 30); do
        if curl -fsS http://127.0.0.1:7000/healthz >/dev/null 2>&1; then
            break
        fi
        sleep 1
    done
    say ""
    say "Installed Setu ${VERSION}"
    say "  binary:  /opt/setu/setu"
    say "  config:  /etc/setu/setu.toml"
    say "  service: setu.service (systemctl status setu)"
fi
# The wizard path is constant: /setup creates the first owner.
setup_ip="$(hostname -I 2>/dev/null | awk '{print $1}')"
[[ -n "$setup_ip" ]] || setup_ip="127.0.0.1"
say "  setup:   http://${setup_ip}:7000/setup"
