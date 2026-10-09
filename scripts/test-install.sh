#!/usr/bin/env bash
# Install-flow test (plan Task 14): run install.sh inside a fresh
# ubuntu:24.04 systemd container against a locally built + locally served
# archive; assert /healthz 200 and /api/v1/setup → needed:true.
#
#   CARGO_BUILD_JOBS=4 bash /home/gopal/Desktop/wp/setu-work-1/scripts/test-install.sh
#
# Needs privileged Docker (systemd in a container) and ~5-10 minutes on
# first run (apt installs PostgreSQL inside the container). Cleans up
# the container and the file server on exit.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
# Unique per run: a shared Docker host may hold other agents' containers,
# and a fixed name invites cross-talk (stale `docker rm -f` or manual
# debugging killing someone else's run). Override with
# SETU_TEST_INSTALL_CTN when dodging a name-based cleaner.
CTN="${SETU_TEST_INSTALL_CTN:-setu-install-test-$(date +%s)-$$}"
FILE_PORT="${SETU_TEST_INSTALL_FILE_PORT:-17002}"
VERSION="${SETU_TEST_INSTALL_VERSION:-0.1.0-test}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}"

say() { printf '%s\n' "$*"; }
fail() { printf 'test-install: %s\n' "$*" >&2; exit 1; }

case "$(uname -m)" in
    x86_64) TARGET="x86_64-unknown-linux-musl" ;;
    aarch64|arm64) TARGET="aarch64-unknown-linux-musl" ;;
    *) fail "no release target for $(uname -m)" ;;
esac

cleanup() {
    status=$?
    if [[ "${SETU_TEST_INSTALL_KEEP:-0}" == "1" ]]; then
        say "keeping container $CTN and stage ${STAGE:-?} (SETU_TEST_INSTALL_KEEP=1)"
        trap - EXIT
        exit "$status"
    fi
    [[ -n "${FILE_PID:-}" ]] && kill "$FILE_PID" 2>/dev/null || true
    docker rm -f "$CTN" >/dev/null 2>&1 || true
    [[ -n "${STAGE:-}" ]] && rm -rf "$STAGE" || true
}
trap cleanup EXIT

docker rm -f "$CTN" >/dev/null 2>&1 || true

say "building setu binary"
(cd "$REPO_ROOT" && cargo build --bin setu) || fail "cargo build --bin setu"

STAGE="$(mktemp -d)"
NAME="setu-${VERSION}-${TARGET}"
mkdir -p "${STAGE}/${NAME}" "${STAGE}/v${VERSION}"
cp "${REPO_ROOT}/target/debug/setu" "${STAGE}/${NAME}/setu"
# Archive layout: one top-level `setu` binary (what install.sh asserts).
tar -czf "${STAGE}/v${VERSION}/${NAME}.tar.gz" -C "${STAGE}/${NAME}" setu
(cd "${STAGE}/v${VERSION}" && sha256sum "${NAME}.tar.gz" > "${NAME}.tar.gz.sha256")
say "staged v${VERSION}/${NAME}.tar.gz ($(du -h "${STAGE}/v${VERSION}/${NAME}.tar.gz" | cut -f1))"

# The archive layout the script asserts on: one top-level `setu` binary.
tar -tzf "${STAGE}/v${VERSION}/${NAME}.tar.gz" | grep -qxE '(\./)?setu' \
    || fail "archive layout wrong: $(tar -tzf "${STAGE}/v${VERSION}/${NAME}.tar.gz")"

# `exec` so $FILE_PID is python itself (not a subshell wrapper): the
# EXIT trap's `kill` must land on the server (an orphaned server keeps
# the port and 404s the next run from its deleted stage dir).
# Bound to 0.0.0.0 (like e2e.sh): the container reaches the host
# through the bridge gateway, never through the host's 127.0.0.1.
(cd "$STAGE" && exec python3 -m http.server "$FILE_PORT" --bind 0.0.0.0 >/dev/null 2>&1) &
FILE_PID=$!
sleep 1
curl -fsS "http://127.0.0.1:${FILE_PORT}/v${VERSION}/${NAME}.tar.gz.sha256" >/dev/null \
    || fail "local file server not serving on :$FILE_PORT"

say "booting systemd container"
docker run -d -t --name "$CTN" --privileged --cgroupns=host \
    -v /sys/fs/cgroup:/sys/fs/cgroup:rw \
    --add-host=host.docker.internal:host-gateway \
    jrei/systemd-ubuntu:24.04 >/dev/null || fail "docker run"
for _ in $(seq 1 60); do
    st="$(docker exec "$CTN" systemctl is-system-running 2>/dev/null || true)"
    [[ "$st" == "running" || "$st" == "degraded" ]] && break
    sleep 1
done
[[ "$st" == "running" || "$st" == "degraded" ]] || fail "systemd never settled (last: $st)"
say "systemd settled ($st)"

docker cp "${REPO_ROOT}/install.sh" "$CTN:/root/install.sh" || fail "docker cp install.sh"

say "running install.sh (apt PostgreSQL inside; takes minutes)"
docker exec -e SETU_RELEASE_BASE_URL="http://host.docker.internal:${FILE_PORT}" \
    "$CTN" bash /root/install.sh --version "$VERSION" --public-url "http://127.0.0.1:7000" \
    || fail "install.sh failed (see container log above)"

# `setu serve` binds loopback by default (spec; Caddy fronts it in
# production), so a host port-publish would forward to the container's
# bridge IP where nothing listens. Assert from inside the container,
# over its own loopback — that is also what the setup wizard uses.
say "asserting panel health"
if ! health="$(docker exec "$CTN" curl -fsS http://127.0.0.1:7000/healthz)"; then
    say "--- container diagnostics (setu.service did not answer) ---"
    docker exec "$CTN" systemctl status setu.service --no-pager 2>&1 | head -20 || true
    docker exec "$CTN" journalctl -u setu.service --no-pager -n 40 2>&1 | tail -40 || true
    fail "/healthz unreachable inside $CTN"
fi
echo "$health" | grep -q '"status"[[:space:]]*:[[:space:]]*"ok"' \
    || fail "/healthz not ok: $health"
say "ok - /healthz 200 {status: ok}"

if ! setup="$(docker exec "$CTN" curl -fsS http://127.0.0.1:7000/api/v1/setup)"; then
    fail "/api/v1/setup unreachable inside $CTN"
fi
echo "$setup" | grep -q '"needed"[[:space:]]*:[[:space:]]*true' \
    || fail "/api/v1/setup not needed:true on a fresh install: $setup"
say "ok - /api/v1/setup needed:true"

say "TEST-INSTALL PASS"
