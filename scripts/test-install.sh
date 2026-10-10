#!/usr/bin/env bash
# Installer test (plan Task 14, spec §9). TAP output, 9 assertions, then
# `INSTALL PASS` or `INSTALL FAIL`.
#
#   SETU_INSTALL_DIR=<dir> bash scripts/test-install.sh > <log> 2>&1
#
# Builds the release binary exactly as the release workflow does
# (x86_64 musl, no `test-root`/`e2e-hooks`), packages it with
# scripts/package-release.sh (archive + install.sh + SHA256SUMS), serves
# the assets on the host's address of the Docker network
# `setu-install-net`, boots a fresh systemd container `setu-install-test`
# (the e2e image, `--privileged --cgroupns=private`, `setu.test` →
# 127.0.0.1 in its /etc/hosts), runs
# `install.sh --tls internal --panel-host setu.test --archive-url … --sums-url …`
# inside it and asserts the panel through Caddy, the setup token, the
# wizard's local-server command, the agent, `setu backup` and an
# idempotent second run.
#
# Host safety (docs/E2E.md): nothing binds 0.0.0.0 or uses port 5432 on
# the host; no host network, no host cgroup mount. Cleanup removes only
# the container, the network and the PID saved under $SETU_INSTALL_DIR.
# SETU_INSTALL_KEEP=1 leaves them for debugging.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
SETU_INSTALL_DIR=${SETU_INSTALL_DIR:-${TMPDIR:-/tmp}/setu-install}
mkdir -p "$SETU_INSTALL_DIR"
SETU_INSTALL_DIR=$(cd "$SETU_INSTALL_DIR" && pwd)

NET=setu-install-net
CTN=setu-install-test
IMAGE=setu-e2e-agent:latest
ART_PORT=8124
TARGET=x86_64-unknown-linux-musl
HOST=setu.test
ORIGIN=https://$HOST
DIST=$SETU_INSTALL_DIR/dist
OWNER_EMAIL=owner@install.test
TOTAL=9

N=0
FAILED=0
WHY=""

note() { echo "# $*"; }
bail() {
  echo "Bail out! $*"
  diag setup || true
  exit 1
}
why() { WHY="$*"; }

run_test() {
  local name=$1 fn=$2
  N=$((N + 1))
  WHY=""
  local t0=$SECONDS
  if "$fn"; then
    echo "ok $N - $name"
  else
    echo "not ok $N - $name"
    echo "#   reason: ${WHY:-unknown}"
    FAILED=$((FAILED + 1))
    diag "$name" || true
  fi
  note "  ($name took $((SECONDS - t0)) s)"
}

kill_saved() {
  local file=$1 pattern=$2 pid
  [ -f "$file" ] || return 0
  pid=$(cat "$file")
  if [ -n "$pid" ] && [ -r "/proc/$pid/cmdline" ] &&
    tr '\0' ' ' <"/proc/$pid/cmdline" | grep -q -- "$pattern"; then
    kill "$pid" 2>/dev/null || true
    for _ in 1 2 3 4 5 6 7 8 9 10; do
      kill -0 "$pid" 2>/dev/null || break
      sleep 0.5
    done
    kill -9 "$pid" 2>/dev/null || true
  fi
  rm -f "$file"
}

teardown() {
  kill_saved "$SETU_INSTALL_DIR/httpd.pid" "http.server"
  docker rm -fv "$CTN" >/dev/null 2>&1 || true
  if docker network inspect "$NET" >/dev/null 2>&1; then
    docker network rm "$NET" >/dev/null 2>&1 || true
  fi
}

on_exit() {
  local rc=$?
  if [ "${SETU_INSTALL_KEEP:-0}" = "1" ]; then
    note "SETU_INSTALL_KEEP=1: leaving $CTN, $NET and the saved PID running"
  else
    teardown
  fi
  if [ $rc -eq 0 ] && [ "$FAILED" -eq 0 ] && [ "$N" -eq "$TOTAL" ]; then
    echo "INSTALL PASS"
  else
    echo "INSTALL FAIL ($FAILED failed, $N of $TOTAL run)"
    [ $rc -ne 0 ] || rc=1
  fi
  exit $rc
}

cx() { docker exec "$CTN" "$@"; }

# capi METHOD PATH: JSON body on stdin (POST) through Caddy inside the
# box with the cookie jar /root/jar; prints the HTTP code, the body
# lands in /root/last.json (read with `last`).
capi() {
  local method=$1 path=$2
  if [ "$method" = GET ]; then
    cx curl -sS -k --max-time 30 --resolve "$HOST:443:127.0.0.1" \
      -b /root/jar -c /root/jar -o /root/last.json -w '%{http_code}' \
      "$ORIGIN/api/v1$path" || true
  else
    docker exec -i "$CTN" curl -sS -k --max-time 30 --resolve "$HOST:443:127.0.0.1" \
      -b /root/jar -c /root/jar -o /root/last.json -w '%{http_code}' \
      -X "$method" -H "Origin: $ORIGIN" -H 'Content-Type: application/json' \
      --data-binary @- "$ORIGIN/api/v1$path" || true
  fi
}
last() { cx cat /root/last.json 2>/dev/null || true; }

wait_for() {
  local timeout=$1
  shift
  local deadline=$((SECONDS + timeout))
  while [ $SECONDS -lt $deadline ]; do
    "$@" >/dev/null 2>&1 && return 0
    sleep 1
  done
  return 1
}

systemd_settled() {
  local st
  st=$(cx systemctl is-system-running 2>/dev/null) || true
  [ "$st" = running ] || [ "$st" = degraded ]
}

diag() {
  local name=$1 file
  file="$SETU_INSTALL_DIR/diag-$(printf '%02d' "$N")-${name//[^a-z0-9_]/_}.txt"
  {
    echo "=== install.log (tail)"
    tail -40 "$SETU_INSTALL_DIR/install.log" 2>/dev/null || true
    if docker ps --format '{{.Names}}' | grep -qx "$CTN"; then
      echo "=== systemctl status"
      cx systemctl --no-pager status setu setu-agent caddy-api postgresql 2>&1 | head -80 || true
      echo "=== journal setu"
      cx journalctl -u setu -n 80 --no-pager 2>&1 || true
      echo "=== journal setu-agent"
      cx journalctl -u setu-agent -n 80 --no-pager 2>&1 || true
      echo "=== journal caddy-api"
      cx journalctl -u caddy-api -n 40 --no-pager 2>&1 || true
      echo "=== ls -l /run/setu"
      cx ls -la /run/setu 2>&1 || true
    fi
  } >"$file" 2>&1
  note "  diagnostics: $file"
  tail -40 "$file" | sed 's/^/#   | /'
}

trap on_exit EXIT

# ================================================================== setup

echo "1..$TOTAL"
note "SETU_INSTALL_DIR=$SETU_INSTALL_DIR"
for tool in docker jq curl python3 cargo; do
  command -v "$tool" >/dev/null 2>&1 || bail "missing prerequisite: $tool"
done
[ "$(stat -fc %T /sys/fs/cgroup)" = cgroup2fs ] || bail "host is not cgroup v2"
FREE_GB=$(df -BG --output=avail / | tail -1 | tr -dc '0-9')
[ "$FREE_GB" -ge 20 ] || bail "less than 20 GB free on /"

teardown
rm -rf "$DIST" "$SETU_INSTALL_DIR"/diag-*.txt
mkdir -p "$DIST"

# ---- the release build: musl, no test features (as release.yml)
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-4}
if ! command -v musl-gcc >/dev/null 2>&1 &&
  ! command -v x86_64-linux-musl-gcc >/dev/null 2>&1 &&
  [ -z "${CC_x86_64_unknown_linux_musl:-}" ]; then
  command -v clang >/dev/null 2>&1 || bail "no C compiler for $TARGET (install musl-tools or clang)"
  export CC_x86_64_unknown_linux_musl=clang
fi
[ -f "$ROOT/ui/dist/index.html" ] || note "ui/dist absent: the binary is built API-only"
note "building setu ($TARGET, release, no features)"
(cd "$ROOT" && cargo build --release --locked --target "$TARGET" -p setu-api --bin setu) \
  >"$SETU_INSTALL_DIR/build.log" 2>&1 || { tail -30 "$SETU_INSTALL_DIR/build.log"; bail "cargo build failed"; }
SETU_BIN=$ROOT/target/$TARGET/release/setu
file "$SETU_BIN" | grep -q 'static' || bail "setu is not statically linked"
VERSION=$("$SETU_BIN" --version | awk '{print $2}')
bash "$ROOT/scripts/package-release.sh" "$VERSION" "$(dirname "$SETU_BIN")" "$DIST" >/dev/null ||
  bail "package-release.sh failed"
ARCHIVE=setu-$VERSION-$TARGET.tar.gz
(cd "$DIST" && sha256sum -c --quiet SHA256SUMS) || bail "SHA256SUMS does not verify"
note "packaged $ARCHIVE + SHA256SUMS"

note "building image $IMAGE"
docker build -q -t "$IMAGE" "$ROOT/scripts/e2e" >"$SETU_INSTALL_DIR/image.log" 2>&1 ||
  { tail -20 "$SETU_INSTALL_DIR/image.log"; bail "docker build failed"; }

# ---- network, asset server on the host's address on it
docker network create "$NET" >/dev/null
HOSTIP=$(docker network inspect "$NET" -f '{{range .IPAM.Config}}{{.Gateway}}{{end}}')
[[ "$HOSTIP" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]] || bail "no gateway address on $NET"
python3 -I -m http.server "$ART_PORT" --bind "$HOSTIP" --directory "$DIST" \
  >"$SETU_INSTALL_DIR/httpd.log" 2>&1 &
echo $! >"$SETU_INSTALL_DIR/httpd.pid"
ASSETS=http://$HOSTIP:$ART_PORT
wait_for 20 curl -sf -o /dev/null "$ASSETS/SHA256SUMS" || bail "asset server unreachable"
note "assets on $ASSETS"

# ---- the fresh box
docker run -d --name "$CTN" --hostname setu-install-test --privileged --cgroupns=private \
  --network "$NET" --add-host "$HOST:127.0.0.1" "$IMAGE" >/dev/null
wait_for 90 systemd_settled || bail "systemd in $CTN never settled"
docker cp "$ROOT/install.sh" "$CTN:/root/install.sh" >/dev/null
note "box up; install.sh copied in"

INSTALL_ARGS=(--tls internal --panel-host "$HOST"
  --archive-url "$ASSETS/$ARCHIVE" --sums-url "$ASSETS/SHA256SUMS")

# ================================================================== assertions

SERVER_ID=""
TOML_SUM=""

t_install_sh_succeeds() {
  if ! timeout 1500 docker exec "$CTN" bash /root/install.sh "${INSTALL_ARGS[@]}" \
    >"$SETU_INSTALL_DIR/install.log" 2>&1; then
    why "install.sh exited non-zero: $(tail -3 "$SETU_INSTALL_DIR/install.log" | tr '\n' ' ')"
    return 1
  fi
  grep -q "Open $ORIGIN/setup" "$SETU_INSTALL_DIR/install.log" ||
    { why "no setup URL in the output"; return 1; }
  local mode owner
  mode=$(cx stat -c '%a %U:%G' /etc/setu/setu.toml)
  [ "$mode" = "600 setu:setu" ] || { why "setu.toml is $mode"; return 1; }
  owner=$(cx stat -c '%a %U:%G' /opt/setu/setu)
  [ "$owner" = "755 root:root" ] || { why "/opt/setu/setu is $owner"; return 1; }
  cx grep -qx 'listen = "unix:/run/setu/http.sock"' /etc/setu/setu.toml ||
    { why "setu.toml does not listen on the unix socket"; return 1; }
  TOML_SUM=$(cx sha256sum /etc/setu/setu.toml | cut -d' ' -f1)
}

t_healthz_through_caddy() {
  local code
  code=$(cx curl -sk -o /dev/null -w '%{http_code}' --max-time 10 \
    --resolve "$HOST:443:127.0.0.1" "$ORIGIN/healthz" || true)
  [ "$code" = 200 ] || { why "healthz: HTTP $code"; return 1; }
  # Only through Caddy: setu serve has no TCP listener.
  cx sh -c 'ss -ltnp | grep -q "setu"' && { why "setu serve listens on TCP"; return 1; }
  return 0
}

t_setup_without_token_401() {
  local code
  code=$(jq -n --arg e "$OWNER_EMAIL" '{token: "not-the-setup-token", email: $e, password: "irrelevant-pw-1"}' |
    capi POST /setup)
  [ "$code" = 401 ] || { why "POST /setup with a wrong token: HTTP $code $(last)"; return 1; }
}

OWNER_PASSWORD=$(head -c 18 /dev/urandom | base64 | tr -d '/+=')
export OWNER_PASSWORD OWNER_EMAIL

t_setup_with_token_201() {
  local code
  cx test -s /var/lib/setu/setup-token || { why "no /var/lib/setu/setup-token"; return 1; }
  code=$(SETUP_TOKEN=$(cx cat /var/lib/setu/setup-token) jq -n \
    '{token: env.SETUP_TOKEN, email: env.OWNER_EMAIL, password: env.OWNER_PASSWORD}' |
    capi POST /setup)
  [ "$code" = 201 ] || { why "POST /setup with the token: HTTP $code $(last)"; return 1; }
  cx test -e /var/lib/setu/setup-token && { why "the token file outlived setup"; return 1; }
  return 0
}

t_login() {
  local code
  code=$(jq -n '{email: env.OWNER_EMAIL, password: env.OWNER_PASSWORD}' | capi POST /auth/login)
  [ "$code" = 200 ] || { why "login: HTTP $code $(last)"; return 1; }
}

server_online() {
  [ "$(capi GET "/servers/$SERVER_ID")" = 200 ] &&
    [ "$(last | jq -r .status)" = online ]
}

t_local_server_joins() {
  local code resp cmd token
  code=$(echo '{"name":"panel"}' | capi POST /setup/local-server)
  [ "$code" = 201 ] || { why "POST /setup/local-server: HTTP $code $(last)"; return 1; }
  resp=$(last)
  SERVER_ID=$(echo "$resp" | jq -r '.server.id // empty')
  cmd=$(echo "$resp" | jq -r '.install_command // empty')
  token=$(echo "$resp" | jq -r '.join_token // empty')
  note "  install command: $cmd"
  [[ "$cmd" =~ ^setu\ agent\ install\ [A-Za-z0-9\ ./:_-]+$ ]] ||
    { why "unexpected install command: $cmd"; return 1; }
  [ -n "$SERVER_ID" ] && [ -n "$token" ] || { why "no server id or join token"; return 1; }
  if ! SETU_JOIN_TOKEN=$token timeout 900 docker exec -e SETU_JOIN_TOKEN "$CTN" sh -c "$cmd" \
    >"$SETU_INSTALL_DIR/agent-install.log" 2>&1; then
    why "the install command failed: $(tail -3 "$SETU_INSTALL_DIR/agent-install.log" | tr '\n' ' ')"
    return 1
  fi
  wait_for 60 server_online || { why "server not online within 60 s: $(last)"; return 1; }
  # The panel still answers through Caddy after the agent rewrote it.
  [ "$(cx curl -sk -o /dev/null -w '%{http_code}' --resolve "$HOST:443:127.0.0.1" "$ORIGIN/healthz")" = 200 ] ||
    { why "panel lost after agent install"; return 1; }
}

t_agent_doctor() {
  cx setu agent doctor >"$SETU_INSTALL_DIR/doctor.log" 2>&1 ||
    { why "doctor failed: $(tr '\n' ' ' <"$SETU_INSTALL_DIR/doctor.log" | tail -c 400)"; return 1; }
}

t_self_backup() {
  cx rm -f /tmp/setu.dump
  docker exec -u setu -w /etc/setu "$CTN" /opt/setu/setu backup --out /tmp/setu.dump \
    >"$SETU_INSTALL_DIR/backup.log" 2>&1 ||
    { why "setu backup failed: $(tail -2 "$SETU_INSTALL_DIR/backup.log" | tr '\n' ' ')"; return 1; }
  local listing
  listing=$(cx pg_restore -l /tmp/setu.dump 2>&1) || { why "pg_restore -l: $listing"; return 1; }
  echo "$listing" | grep -q 'TABLE public operators' ||
    { why "no operators table in the dump listing"; return 1; }
}

t_rerun_is_noop() {
  local pid_before pid_after agent_before agent_after sum
  pid_before=$(cx systemctl show -p MainPID --value setu)
  agent_before=$(cx systemctl show -p MainPID --value setu-agent)
  if ! timeout 900 docker exec "$CTN" bash /root/install.sh "${INSTALL_ARGS[@]}" \
    >"$SETU_INSTALL_DIR/install-2.log" 2>&1; then
    why "second install.sh failed: $(tail -3 "$SETU_INSTALL_DIR/install-2.log" | tr '\n' ' ')"
    return 1
  fi
  grep -q "Keeping /etc/setu/setu.toml" "$SETU_INSTALL_DIR/install-2.log" ||
    { why "second run did not keep setu.toml"; return 1; }
  sum=$(cx sha256sum /etc/setu/setu.toml | cut -d' ' -f1)
  [ "$sum" = "$TOML_SUM" ] || { why "setu.toml changed"; return 1; }
  pid_after=$(cx systemctl show -p MainPID --value setu)
  [ "$pid_before" = "$pid_after" ] || { why "setu.service restarted ($pid_before → $pid_after)"; return 1; }
  agent_after=$(cx systemctl show -p MainPID --value setu-agent)
  [ "$agent_before" = "$agent_after" ] ||
    { why "setu-agent.service restarted with the binary unchanged ($agent_before → $agent_after)"; return 1; }
  grep -q "Setup is complete" "$SETU_INSTALL_DIR/install-2.log" ||
    { why "second run offered setup again"; return 1; }
  server_online || { why "server not online after the second run"; return 1; }
}

run_test install_sh_succeeds t_install_sh_succeeds
run_test healthz_through_caddy t_healthz_through_caddy
run_test setup_without_token_401 t_setup_without_token_401
run_test setup_with_token_201 t_setup_with_token_201
run_test login t_login
run_test local_server_joins t_local_server_joins
run_test agent_doctor t_agent_doctor
run_test self_backup t_self_backup
run_test rerun_is_noop t_rerun_is_noop
