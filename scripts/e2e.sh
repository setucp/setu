#!/usr/bin/env bash
# Setu end-to-end test (plan Task 11, spec §9). TAP output, 14 assertions,
# then `E2E PASS` or `E2E FAIL`.
#
#   SETU_E2E_DIR=<dir> bash scripts/e2e.sh > <log> 2>&1
#
# What runs where (host safety, see docs/E2E.md):
# - Docker network `setu-e2e-net` (created and removed here). The host's
#   address on it ($HOSTIP, the bridge gateway) is the only host address
#   anything binds: `setu serve` on $HOSTIP:7010, the artefact server on
#   $HOSTIP:8123, the control-plane Postgres published on $HOSTIP:55441.
#   Nothing binds 0.0.0.0 on the host; port 5432 is never used.
# - Containers `setu-e2e-pg` (postgres:16-alpine) and `setu-e2e` (systemd,
#   `--privileged --cgroupns=private`, no host cgroup mount, no host
#   network). Cleanup removes only these two, the network, and the PIDs
#   saved under $SETU_E2E_DIR (never anything by name).
#
# Requirements: Docker with privileged containers, cgroup v2, jq, curl,
# openssl, python3, the x86_64-unknown-linux-musl Rust target (and a C
# compiler for it: musl-gcc, or clang which the script selects itself).
#
# SETU_E2E_KEEP=1 skips cleanup on exit (debugging); the next run removes
# the leftovers first.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
SETU_E2E_DIR=${SETU_E2E_DIR:-/tmp/claude-1000/-home-gopal-Desktop-wp/a208dcfa-eb02-4ac4-8268-ba82cf4b2d26/scratchpad/e2e}
mkdir -p "$SETU_E2E_DIR"
SETU_E2E_DIR=$(cd "$SETU_E2E_DIR" && pwd)

NET=setu-e2e-net
CTN=setu-e2e
PG=setu-e2e-pg
IMAGE=setu-e2e-agent:latest
SERVE_PORT=7010
PG_PORT=55441
ART_PORT=8123
TARGET=x86_64-unknown-linux-musl
OWNER_EMAIL=owner@e2e.test
JAR=$SETU_E2E_DIR/cookies.txt
ART_DIR=$SETU_E2E_DIR/artefacts
DATA_DIR=$SETU_E2E_DIR/data
TOTAL=14

N=0
FAILED=0
WHY=""

# ------------------------------------------------------------------ output

note() { echo "# $*"; }
bail() {
  echo "Bail out! $*"
  diag "setup" || true
  exit 1
}
why() { WHY="$*"; }

# run_test <name> <function>: one TAP line; on failure the reason and a
# diagnostic dump. Assertion functions return non-zero with `why` set.
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

# ------------------------------------------------------------------ cleanup

# Kill a PID saved under $SETU_E2E_DIR, only when its command line still
# matches what we started (a stale file must never kill a reused PID).
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
  kill_saved "$SETU_E2E_DIR/serve.pid" "setu serve"
  kill_saved "$SETU_E2E_DIR/httpd.pid" "http.server"
  docker rm -fv "$CTN" "$PG" >/dev/null 2>&1 || true
  if docker network inspect "$NET" >/dev/null 2>&1; then
    docker network rm "$NET" >/dev/null 2>&1 || true
  fi
}

on_exit() {
  local rc=$?
  if [ "${SETU_E2E_KEEP:-0}" = "1" ]; then
    note "SETU_E2E_KEEP=1: leaving $CTN, $PG, $NET and the saved PIDs running"
  else
    teardown
  fi
  if [ $rc -eq 0 ] && [ "$FAILED" -eq 0 ] && [ "$N" -eq "$TOTAL" ]; then
    echo "E2E PASS"
  else
    echo "E2E FAIL ($FAILED failed, $N of $TOTAL run)"
    [ $rc -ne 0 ] || rc=1
  fi
  exit $rc
}

# ------------------------------------------------------------------ helpers

cx() { docker exec "$CTN" "$@"; }

API_BASE=""   # http://$HOSTIP:7010
api() { # api METHOD PATH [json-body-from-stdin]
  local method=$1 path=$2
  if [ "$method" = GET ]; then
    curl -sS --max-time 30 -b "$JAR" "$API_BASE/api/v1$path"
  else
    curl -sS --max-time 30 -b "$JAR" -c "$JAR" -X "$method" \
      -H "Origin: $API_BASE" -H 'Content-Type: application/json' \
      --data-binary @- "$API_BASE/api/v1$path"
  fi
}
# Same, printing `<http code>` on the last line.
api_code() {
  local method=$1 path=$2
  curl -sS --max-time 30 -o "$SETU_E2E_DIR/last.json" -w '%{http_code}' \
    -b "$JAR" -c "$JAR" -X "$method" -H "Origin: $API_BASE" \
    -H 'Content-Type: application/json' --data-binary @- "$API_BASE/api/v1$path"
}

# GET https://<domain><path> inside the container through Caddy.
tls_get() {
  cx curl -sk --max-time 10 --resolve "$1:443:127.0.0.1" "https://$1$2"
}
tls_code() {
  cx curl -sk -o /dev/null -w '%{http_code}' --max-time 10 \
    --resolve "$1:443:127.0.0.1" "https://$1$2" || true
}
tls_post_json() { # domain path json
  printf '%s' "$3" | docker exec -i "$CTN" curl -sk --max-time 10 \
    --resolve "$1:443:127.0.0.1" -H 'Content-Type: application/json' \
    --data-binary @- "https://$1$2"
}

job_status() { api GET "/jobs/$1" | jq -r '.status // empty' 2>/dev/null || true; }

# wait_job <id> <timeout s>: until done|failed; prints the final status.
wait_job() {
  local id=$1 timeout=$2 st="" deadline=$((SECONDS + $2))
  while [ $SECONDS -lt $deadline ]; do
    st=$(job_status "$id")
    case "$st" in done | failed) break ;; esac
    sleep 1
  done
  echo "${st:-unknown}"
}

site_field() { api GET "/sites/$1" | jq -r "$2" 2>/dev/null || true; }

# wait_site <id> <status> <timeout s>
wait_site() {
  local deadline=$((SECONDS + $3))
  while [ $SECONDS -lt $deadline ]; do
    [ "$(site_field "$1" .status)" = "$2" ] && return 0
    sleep 1
  done
  return 1
}

# Poll a command until it succeeds (timeout s).
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

short_of() { site_field "$1" '.db_name | sub("^site_"; "")'; }

create_site() { # name app version domain → sets SITE_ID JOB_ID
  local body
  body=$(jq -n --arg name "$1" --arg app "$2" --arg v "$3" --arg d "$4" \
    --arg plan "$PLAN_ID" --arg srv "$SERVER_ID" --arg email "$OWNER_EMAIL" \
    '{name: $name, app_id: $app, version: $v, plan_id: $plan, server_id: $srv,
      domain: $d, owner_email: $email, prompts: {}}')
  local resp
  resp=$(printf '%s' "$body" | api POST /sites)
  SITE_ID=$(echo "$resp" | jq -r '.site.id // empty')
  JOB_ID=$(echo "$resp" | jq -r '.job.id // empty')
  if [ -z "$SITE_ID" ] || [ -z "$JOB_ID" ]; then
    why "POST /sites $4: $resp"
    return 1
  fi
}

systemd_settled() {
  local st
  st=$(cx systemctl is-system-running 2>/dev/null) || true
  [ "$st" = running ] || [ "$st" = degraded ]
}

# Diagnostics for a failure: written to a file, its tail printed as TAP
# comments. Secrets never appear here: job logs and agent logs are
# redacted at the source, and no env file is read.
diag() {
  local name=$1 file
  file="$SETU_E2E_DIR/diag-$(printf '%02d' "$N")-${name//[^a-z0-9_]/_}.txt"
  {
    echo "=== serve.log (tail)"
    tail -40 "$SETU_E2E_DIR/serve.log" 2>/dev/null || true
    if docker ps --format '{{.Names}}' | grep -qx "$CTN"; then
      echo "=== systemctl status"
      cx systemctl --no-pager status setu-agent caddy-api 'setu-site@*' 2>&1 | head -80 || true
      echo "=== journal setu-agent"
      cx journalctl -u setu-agent -n 200 --no-pager 2>&1 || true
      echo "=== journal setu-site@*"
      cx journalctl -u 'setu-site@*' -n 60 --no-pager 2>&1 || true
      echo "=== state.json"
      cx cat /var/lib/setu/state.json 2>&1 | jq -c '.sites' 2>/dev/null || true
      echo "=== caddy routes"
      cx curl -s --max-time 5 --unix-socket /run/caddy/admin.sock \
        http://localhost/config/apps/http/servers 2>&1 | head -c 4000 || true
      echo
    fi
    for j in ${LAST_JOBS:-}; do
      echo "=== job $j"
      api GET "/jobs/$j" | jq -r '"status=\(.status) error=\(.error)\n\(.log)"' 2>/dev/null || true
    done
  } >"$file" 2>&1
  note "  diagnostics: $file"
  tail -60 "$file" | sed 's/^/#   | /'
}

trap on_exit EXIT

# ================================================================== setup

echo "1..$TOTAL"
note "SETU_E2E_DIR=$SETU_E2E_DIR"

for tool in docker jq curl openssl python3 cargo; do
  command -v "$tool" >/dev/null 2>&1 || bail "missing prerequisite: $tool"
done
[ "$(stat -fc %T /sys/fs/cgroup)" = cgroup2fs ] || bail "host is not cgroup v2"
FREE_GB=$(df -BG --output=avail / | tail -1 | tr -dc '0-9')
[ "$FREE_GB" -ge 20 ] || bail "less than 20 GB free on /"
INOTIFY=$(cat /proc/sys/fs/inotify/max_user_instances)
[ "$INOTIFY" -ge 1024 ] ||
  note "warning: fs.inotify.max_user_instances=$INOTIFY; systemd in the container may fail"

# Leftovers of an earlier run (ours only).
teardown
rm -rf "$DATA_DIR" "$ART_DIR" "$JAR" "$SETU_E2E_DIR"/diag-*.txt
mkdir -p "$DATA_DIR" "$ART_DIR"

# ---- build (musl, features test-root + e2e-hooks; never in releases)
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-4}
if ! command -v musl-gcc >/dev/null 2>&1 &&
  ! command -v x86_64-linux-musl-gcc >/dev/null 2>&1 &&
  [ -z "${CC_x86_64_unknown_linux_musl:-}" ]; then
  command -v clang >/dev/null 2>&1 || bail "no C compiler for $TARGET (install musl-tools or clang)"
  export CC_x86_64_unknown_linux_musl=clang
fi
note "building setu + hello ($TARGET, release)"
# From $ROOT so rustup applies its rust-toolchain.toml (channel + musl target).
(cd "$ROOT" && cargo build --release --target "$TARGET" \
  --features setu-api/test-root,setu-api/e2e-hooks -p setu-api -p hello) \
  >"$SETU_E2E_DIR/build.log" 2>&1 || { tail -30 "$SETU_E2E_DIR/build.log"; bail "cargo build failed"; }
SETU_BIN=$ROOT/target/$TARGET/release/setu
HELLO_BIN=$ROOT/target/$TARGET/release/hello
file "$SETU_BIN" | grep -q 'static' || bail "setu is not statically linked"

note "building image $IMAGE"
docker build -q -t "$IMAGE" "$ROOT/scripts/e2e" >"$SETU_E2E_DIR/image.log" 2>&1 ||
  { tail -20 "$SETU_E2E_DIR/image.log"; bail "docker build failed"; }

# ---- network and control-plane Postgres
docker network create "$NET" >/dev/null
HOSTIP=$(docker network inspect "$NET" -f '{{range .IPAM.Config}}{{.Gateway}}{{end}}')
[[ "$HOSTIP" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]] || bail "no gateway address on $NET"
API_BASE="http://$HOSTIP:$SERVE_PORT"
note "host address on $NET: $HOSTIP"

PG_PASSWORD=$(openssl rand -hex 16)
POSTGRES_PASSWORD=$PG_PASSWORD docker run -d --name "$PG" --network "$NET" \
  -e POSTGRES_USER=setu -e POSTGRES_PASSWORD -e POSTGRES_DB=setu_e2e \
  -p "$HOSTIP:$PG_PORT:5432" postgres:16-alpine >/dev/null
wait_for 60 docker exec "$PG" pg_isready -U setu -d setu_e2e -h 127.0.0.1 ||
  bail "control-plane Postgres not ready"

# ---- setu serve (bound to $HOSTIP only)
export SETU_LISTEN="$HOSTIP:$SERVE_PORT"
export SETU_PUBLIC_URL="$API_BASE"
export SETU_DATABASE_URL="postgres://setu:$PG_PASSWORD@$HOSTIP:$PG_PORT/setu_e2e"
SETU_SECRET_KEY=$(openssl rand -hex 32)
export SETU_SECRET_KEY
export SETU_DATA_DIR="$DATA_DIR"
"$SETU_BIN" serve >"$SETU_E2E_DIR/serve.log" 2>&1 &
echo $! >"$SETU_E2E_DIR/serve.pid"
unset SETU_DATABASE_URL SETU_SECRET_KEY
wait_for 60 curl -sf "$API_BASE/healthz" || bail "setu serve: no /healthz"
note "setu serve on $API_BASE (pid $(cat "$SETU_E2E_DIR/serve.pid"))"

# ---- setup with the token file, then log in
[ -s "$DATA_DIR/setup-token" ] || bail "no setup token at $DATA_DIR/setup-token"
OWNER_PASSWORD=$(openssl rand -base64 24 | tr -d '/+=')
export OWNER_PASSWORD OWNER_EMAIL
CODE=$(SETUP_TOKEN=$(cat "$DATA_DIR/setup-token") jq -n \
  '{token: env.SETUP_TOKEN, email: env.OWNER_EMAIL, password: env.OWNER_PASSWORD}' |
  api_code POST /setup)
[ "$CODE" = 201 ] || bail "POST /setup: HTTP $CODE $(cat "$SETU_E2E_DIR/last.json")"
CODE=$(jq -n '{email: env.OWNER_EMAIL, password: env.OWNER_PASSWORD}' | api_code POST /auth/login)
[ "$CODE" = 200 ] || bail "login: HTTP $CODE"
note "owner created with the setup token and logged in"

# ---- server row and join token
RESP=$(echo '{"name":"e2e"}' | api POST /servers)
SERVER_ID=$(echo "$RESP" | jq -r '.server.id // empty')
JOIN_TOKEN=$(echo "$RESP" | jq -r '.join_token // empty')
[ -n "$SERVER_ID" ] && [ -n "$JOIN_TOKEN" ] || bail "POST /servers failed"
echo "$RESP" | jq -r .install_command | grep -qF -f <(printf '%s\n' "$JOIN_TOKEN") &&
  bail "the install command embeds the join token"

# ---- the agent box
docker run -d --name "$CTN" --hostname setu-e2e --privileged --cgroupns=private \
  --network "$NET" "$IMAGE" >/dev/null
wait_for 90 systemd_settled || bail "systemd in $CTN never settled"
CTN_IP=$(docker inspect -f "{{(index .NetworkSettings.Networks \"$NET\").IPAddress}}" "$CTN")
cx mkdir -p /opt/setu
docker cp "$SETU_BIN" "$CTN:/opt/setu/setu" >/dev/null
cx chown root:root /opt/setu/setu
cx chmod 0755 /opt/setu/setu

note "setu agent install (apt: postgresql-16, caddy, nftables …)"
SETU_JOIN_TOKEN=$JOIN_TOKEN timeout 900 docker exec -e SETU_JOIN_TOKEN "$CTN" \
  /opt/setu/setu agent install --control-plane "$API_BASE" --insecure --tls internal \
  >"$SETU_E2E_DIR/install.log" 2>&1 ||
  { tail -30 "$SETU_E2E_DIR/install.log"; bail "agent install failed"; }
unset JOIN_TOKEN

# Reconcile every 10 s for `reconcile_repairs_lost_autosave` (spec §9).
cx mkdir -p /etc/systemd/system/setu-agent.service.d
printf '[Service]\nEnvironment=SETU_AGENT_RECONCILE_SECS=10\n' |
  docker exec -i "$CTN" tee /etc/systemd/system/setu-agent.service.d/e2e-reconcile.conf >/dev/null
cx systemctl daemon-reload
cx systemctl restart setu-agent

server_online() {
  local j
  j=$(api GET "/servers/$SERVER_ID") || return 1
  [ "$(echo "$j" | jq -r .status)" = online ] && [ "$(echo "$j" | jq -r .mem_free_mb)" != null ]
}
wait_for 120 server_online || bail "server never online"
cx /opt/setu/setu agent doctor >"$SETU_E2E_DIR/doctor.log" 2>&1 ||
  { cat "$SETU_E2E_DIR/doctor.log"; bail "setu agent doctor failed"; }
note "agent joined, online, doctor exit 0"

# ---- hello archives (one binary; the version comes from the manifest env)
for v in 0.1.0 0.1.1 0.1.2; do
  stage=$SETU_E2E_DIR/stage-$v
  rm -rf "$stage"
  mkdir -p "$stage"
  install -m 0755 "$HELLO_BIN" "$stage/hello"
  echo "$v" >"$stage/VERSION"
  tar -C "$stage" --owner=0 --group=0 --numeric-owner -czf "$ART_DIR/hello-$v.tar.gz" hello VERSION
  sha=$(sha256sum "$ART_DIR/hello-$v.tar.gz" | cut -d' ' -f1)
  extra=""
  [ "$v" = 0.1.2 ] && extra='HELLO_FAIL = { value = "1" }'
  sed -e "s|__VERSION__|$v|g" -e "s|__URL__|http://$HOSTIP:$ART_PORT/hello-$v.tar.gz|" \
    -e "s|__SHA256__|$sha|" -e "s|__EXTRA_ENV__|$extra|" \
    "$ROOT/apps/hello/manifest.tmpl.toml" >"$SETU_E2E_DIR/hello-$v.toml"
done
python3 -I -m http.server "$ART_PORT" --bind "$HOSTIP" --directory "$ART_DIR" \
  >"$SETU_E2E_DIR/httpd.log" 2>&1 &
echo $! >"$SETU_E2E_DIR/httpd.pid"
wait_for 20 curl -sf -o /dev/null "http://$HOSTIP:$ART_PORT/hello-0.1.0.tar.gz" ||
  bail "artefact server unreachable"

for v in 0.1.0 0.1.1 0.1.2; do
  RESP=$(jq -n --rawfile t "$SETU_E2E_DIR/hello-$v.toml" '{toml: $t}' | api POST /apps)
  [ "$(echo "$RESP" | jq -r '.version.version // empty')" = "$v" ] ||
    bail "POST /apps hello $v: $RESP"
done
PLAN_ID=$(api GET /plans | jq -r '.plans[] | select(.name == "Default") | .id' | head -1)
[ -n "$PLAN_ID" ] || bail "no Default plan"
note "hello 0.1.0/0.1.1/0.1.2 published; plan $PLAN_ID"

# ================================================================== assertions

HELLO_ID=""
HELLO_SHORT=""
TWO_ID=""
VYASA_ID=""

t_provision_under_60s() {
  local t0 t1
  t0=$(date +%s.%N)
  create_site hello hello 0.1.0 hello.test || return 1
  HELLO_ID=$SITE_ID
  LAST_JOBS=$JOB_ID
  local st
  st=$(wait_job "$JOB_ID" 120)
  [ "$st" = done ] || { why "provision job $st"; return 1; }
  wait_site "$HELLO_ID" running 10 || { why "site not running"; return 1; }
  t1=$(date +%s.%N)
  local secs
  secs=$(python3 -c "print(round($t1 - $t0, 1))")
  note "  provisioned in ${secs} s"
  HELLO_SHORT=$(short_of "$HELLO_ID")
  python3 -c "import sys; sys.exit(0 if $secs < 60 else 1)" || { why "took $secs s"; return 1; }
}

t_serves_over_tls() {
  local body
  body=$(tls_get hello.test /) || true
  note "  GET https://hello.test/ → $body"
  [[ "$body" == "hello from e2e v0.1.0" ]] || { why "body: $body"; return 1; }
}

t_ownership_and_modes() {
  local d=/srv/setu/sites/$HELLO_SHORT got
  [ -n "$HELLO_SHORT" ] || { why "no site"; return 1; }
  got=$(cx stat -c '%U:%G %a' "$d/env") || true
  [ "$got" = "root:root 600" ] || { why "env: $got"; return 1; }
  got=$(cx stat -c '%U %a' "$d/data") || true
  [ "$got" = "setu-site-$HELLO_SHORT 750" ] || { why "data: $got"; return 1; }
  got=$(cx stat -c '%U:%G %a' "$d/bin") || true
  [ "$got" = "root:root 755" ] || { why "bin: $got"; return 1; }
}

t_public_bind_refused_by_firewall() {
  # A process in the box listening on 0.0.0.0 inside the site port range.
  cx sh -c 'nohup python3 -m http.server 25000 --bind 0.0.0.0 >/dev/null 2>&1 & echo $! >/run/e2e-listener.pid'
  wait_for 10 cx sh -c "ss -ltn | grep -q '0.0.0.0:25000'" ||
    { why "listener never came up"; return 1; }
  # Control: it answers on loopback inside the box.
  cx curl -sf --max-time 3 -o /dev/null http://127.0.0.1:25000/ ||
    { why "listener does not answer on loopback"; return 1; }
  local rc=0
  curl -s --max-time 3 -o /dev/null "http://$CTN_IP:25000/" || rc=$?
  cx sh -c 'kill "$(cat /run/e2e-listener.pid)"; rm -f /run/e2e-listener.pid' || true
  [ $rc -ne 0 ] || { why "host reached $CTN_IP:25000"; return 1; }
  cx nft list table inet setu >/dev/null || { why "nft table inet setu missing"; return 1; }
}

t_caddy_admin_closed() {
  if cx curl -s --max-time 2 -o /dev/null http://localhost:2019/config/; then
    why "TCP 2019 answered"
    return 1
  fi
  # Control: root can use the socket.
  cx curl -sf --max-time 3 -o /dev/null --unix-socket /run/caddy/admin.sock http://x/config/ ||
    { why "admin socket unusable even for root"; return 1; }
  if docker exec -u "setu-site-$HELLO_SHORT" "$CTN" \
    curl -sf --max-time 3 -o /dev/null --unix-socket /run/caddy/admin.sock http://x/config/; then
    why "site user opened the admin socket"
    return 1
  fi
}

t_limits_and_metrics() {
  local mem
  mem=$(cx systemctl show -p MemoryMax --value "setu-site@$HELLO_SHORT.service") || true
  [ "$mem" = 536870912 ] || { why "MemoryMax=$mem"; return 1; }
  local deadline=$((SECONDS + 90)) m=""
  while [ $SECONDS -lt $deadline ]; do
    m=$(api GET "/sites/$HELLO_ID" | jq -c '.metrics')
    if echo "$m" | jq -e '.memory_bytes > 0 and .disk_bytes > 0' >/dev/null 2>&1; then
      note "  metrics: $m"
      return 0
    fi
    sleep 3
  done
  why "metrics after 90 s: $m"
  return 1
}

t_backup_restore() {
  local r st bid
  r=$(tls_post_json hello.test /notes '{"text":"a"}') || true
  echo "$r" | jq -e '.text == "a"' >/dev/null 2>&1 || { why "POST /notes a: $r"; return 1; }
  r=$(echo '{}' | api POST "/sites/$HELLO_ID/backups")
  LAST_JOBS=$(echo "$r" | jq -r '.job.id // empty')
  [ -n "$LAST_JOBS" ] || { why "POST backups: $r"; return 1; }
  st=$(wait_job "$LAST_JOBS" 120)
  [ "$st" = done ] || { why "backup job $st"; return 1; }
  bid=$(api GET "/sites/$HELLO_ID/backups" |
    jq -r '[.backups[] | select(.kind == "manual" and .status == "completed")][0].id // empty')
  [ -n "$bid" ] || { why "no completed manual backup listed"; return 1; }
  r=$(tls_post_json hello.test /notes '{"text":"b"}') || true
  [ "$(tls_get hello.test /notes)" = '["a","b"]' ] || { why "notes before restore"; return 1; }
  [ "$(tls_get hello.test /uploads)" = '["note-1.txt","note-2.txt"]' ] ||
    { why "uploads before restore: $(tls_get hello.test /uploads)"; return 1; }
  r=$(echo '{}' | api POST "/sites/$HELLO_ID/backups/$bid/restore")
  LAST_JOBS=$(echo "$r" | jq -r '.job.id // empty')
  [ -n "$LAST_JOBS" ] || { why "POST restore: $r"; return 1; }
  st=$(wait_job "$LAST_JOBS" 180)
  [ "$st" = done ] || { why "restore job $st"; return 1; }
  wait_site "$HELLO_ID" running 30 || { why "site not running after restore"; return 1; }
  local notes uploads
  wait_for 30 sh -c "docker exec $CTN curl -skf --max-time 5 --resolve hello.test:443:127.0.0.1 https://hello.test/notes" || true
  notes=$(tls_get hello.test /notes) || true
  uploads=$(tls_get hello.test /uploads) || true
  note "  after restore: notes=$notes uploads=$uploads"
  [ "$notes" = '["a"]' ] || { why "notes after restore: $notes"; return 1; }
  [ "$uploads" = '["note-1.txt"]' ] || { why "uploads after restore: $uploads"; return 1; }
}

update_to() { # site version → echoes the job id
  local r
  r=$(jq -n --arg v "$2" '{version: $v}' | api POST "/sites/$1/update")
  echo "$r" | jq -r '.job.id // empty'
}

t_update_to_0_1_1() {
  local j st body tables
  j=$(update_to "$HELLO_ID" 0.1.1)
  LAST_JOBS=$j
  [ -n "$j" ] || { why "POST update refused"; return 1; }
  st=$(wait_job "$j" 180)
  [ "$st" = done ] || { why "update job $st"; return 1; }
  [ "$(site_field "$HELLO_ID" .status)" = running ] || { why "site not running"; return 1; }
  [ "$(site_field "$HELLO_ID" .app_version)" = 0.1.1 ] || { why "app_version not 0.1.1"; return 1; }
  body=$(tls_get hello.test /) || true
  [ "$body" = "hello from e2e v0.1.1" ] || { why "GET / → $body"; return 1; }
  tables=$(cx sudo -u postgres psql -X -d "site_$HELLO_SHORT" -c '\dt') || true
  echo "$tables" | grep -qw notes_v2 || { why "notes_v2 missing: $tables"; return 1; }
}

t_rollback_restores_database() {
  local j st err tables notes body
  j=$(update_to "$HELLO_ID" 0.1.2)
  LAST_JOBS=$j
  [ -n "$j" ] || { why "POST update refused"; return 1; }
  st=$(wait_job "$j" 300)
  [ "$st" = failed ] || { why "update job $st (want failed)"; return 1; }
  err=$(api GET "/jobs/$j" | jq -r '.error // empty')
  note "  job error: $err"
  [[ "$err" == rollback:* ]] || { why "error is not a completed rollback: $err"; return 1; }
  [ "$(site_field "$HELLO_ID" .status)" = running ] || { why "site not running"; return 1; }
  [ "$(site_field "$HELLO_ID" .app_version)" = 0.1.1 ] || { why "app_version not 0.1.1"; return 1; }
  tables=$(cx sudo -u postgres psql -X -d "site_$HELLO_SHORT" -c '\dt') || true
  if echo "$tables" | grep -qw notes_v3; then why "notes_v3 survived the rollback"; return 1; fi
  echo "$tables" | grep -qw notes_v2 || { why "notes_v2 gone"; return 1; }
  notes=$(tls_get hello.test /notes) || true
  [ "$notes" = '["a"]' ] || { why "notes after rollback: $notes"; return 1; }
  body=$(tls_get hello.test /) || true
  [ "$body" = "hello from e2e v0.1.1" ] || { why "GET / → $body"; return 1; }
}

DROPIN=/etc/systemd/system/setu-agent.service.d/e2e-exit.conf
t_agent_exit_mid_provision_recovers() {
  # The agent exits after persisting the write_unit result and before
  # posting it; Restart=no keeps it down until the script restarts it.
  printf '[Service]\nEnvironment=SETU_AGENT_TEST_EXIT_AFTER=write_unit\nRestart=no\n' |
    docker exec -i "$CTN" tee "$DROPIN" >/dev/null
  cx systemctl daemon-reload
  cx systemctl restart setu-agent
  local since
  since=$(cx date '+%Y-%m-%d %H:%M:%S')
  create_site two hello 0.1.0 two.test || return 1
  TWO_ID=$SITE_ID
  LAST_JOBS=$JOB_ID
  local deadline=$((SECONDS + 120)) state=""
  while [ $SECONDS -lt $deadline ]; do
    state=$(cx systemctl is-active setu-agent 2>/dev/null) || true
    case "$state" in inactive | failed) break ;; esac
    sleep 1
  done
  cx rm -f "$DROPIN"
  cx systemctl daemon-reload
  case "$state" in inactive | failed) ;; *)
    cx systemctl restart setu-agent || true
    why "agent never exited (state: $state)"
    return 1
    ;;
  esac
  note "  agent exited after write_unit (unit $state)"
  cx systemctl restart setu-agent
  local st
  st=$(wait_job "$JOB_ID" 180)
  [ "$st" = done ] || { why "provision job $st"; return 1; }
  wait_site "$TWO_ID" running 10 || { why "two.test not running"; return 1; }
  local n
  n=$(api GET "/jobs/$JOB_ID" | jq -r '.log' | grep -c '^step write_unit: ' || true)
  [ "$n" = 1 ] || { why "job log has $n write_unit steps"; return 1; }
  local journal cmd
  journal=$(cx journalctl -u setu-agent --since "$since" --no-pager -o cat) || true
  cmd=$(echo "$journal" | grep 'command received' | grep 'write_unit' | grep -o 'cmd=[0-9A-Za-z]*' | tail -1)
  [ -n "$cmd" ] || { why "no write_unit command in the agent journal"; return 1; }
  echo "$journal" | grep "cached result" | grep -q -- "$cmd" ||
    { why "no cached-result line for $cmd"; return 1; }
  echo "$journal" | grep -- "$cmd" | grep -v 'pam_unix' | cut -c1-200 | sed 's/^/#     journal: /'
  note "  $cmd: executed once, cached result posted after restart"
  [ "$(tls_get two.test /)" = "hello from e2e v0.1.0" ] || { why "two.test does not serve"; return 1; }
}

t_delete_site() {
  local short=$HELLO_SHORT r j st code
  r=$(echo '{}' | api DELETE "/sites/$HELLO_ID")
  j=$(echo "$r" | jq -r '.job.id // empty')
  LAST_JOBS=$j
  [ -n "$j" ] || { why "DELETE site: $r"; return 1; }
  local deadline=$((SECONDS + 180))
  while [ $SECONDS -lt $deadline ]; do
    code=$(curl -s -o /dev/null -w '%{http_code}' -b "$JAR" "$API_BASE/api/v1/sites/$HELLO_ID")
    [ "$code" = 404 ] && break
    sleep 2
  done
  [ "$code" = 404 ] || { why "site still there (job $(job_status "$j"))"; return 1; }
  if cx systemctl is-active --quiet "setu-site@$short.service"; then why "unit still active"; return 1; fi
  if cx test -e "/etc/systemd/system/setu-site@$short.service.d"; then why "unit drop-in remains"; return 1; fi
  if cx systemctl status "setu-site@$short.service" >/dev/null 2>&1; then why "systemctl status still succeeds"; return 1; fi
  if cx id "setu-site-$short" >/dev/null 2>&1; then why "user setu-site-$short remains"; return 1; fi
  if cx sudo -u postgres psql -X -lqt | cut -d'|' -f1 | grep -qw "site_$short"; then
    why "database site_$short remains"
    return 1
  fi
  if cx test -e "/srv/setu/sites/$short"; then why "site directory remains"; return 1; fi
  local bid
  bid=$(api GET /backups |
    jq -r '[.backups[] | select(.kind == "pre_delete" and .domain == "hello.test")][0].id // empty')
  [ -n "$bid" ] || { why "pre_delete backup not listed with its domain"; return 1; }
  cx test -d "/var/lib/setu/backups/$short/$bid" || { why "backup files missing before delete"; return 1; }
  r=$(echo '{}' | api DELETE "/backups/$bid")
  j=$(echo "$r" | jq -r '.job.id // empty')
  LAST_JOBS="$LAST_JOBS $j"
  [ -n "$j" ] || { why "DELETE backup: $r"; return 1; }
  st=$(wait_job "$j" 60)
  [ "$st" = done ] || { why "delete_backup job $st"; return 1; }
  if cx test -e "/var/lib/setu/backups/$short/$bid"; then why "backup files remain"; return 1; fi
  if api GET /backups | jq -e --arg b "$bid" '.backups[] | select(.id == $b)' >/dev/null; then
    why "deleted backup still listed"
    return 1
  fi
}

t_vyasa_0_2_0() {
  local r t0 secs st
  r=$(jq -n --rawfile t "$ROOT/crates/manifest/tests/corpus/vyasa_0_2_0.toml" '{toml: $t}' | api POST /apps)
  [ "$(echo "$r" | jq -r '.version.version // empty')" = 0.2.0 ] || { why "POST /apps vyasa: $r"; return 1; }
  t0=$SECONDS
  create_site vyasa vyasa 0.2.0 vyasa.test || return 1
  VYASA_ID=$SITE_ID
  LAST_JOBS=$JOB_ID
  st=$(wait_job "$JOB_ID" 120)
  [ "$st" = done ] || { why "provision job $st"; return 1; }
  wait_site "$VYASA_ID" running 10 || { why "vyasa not running"; return 1; }
  secs=$((SECONDS - t0))
  note "  vyasa running after ${secs} s"
  [ "$secs" -lt 60 ] || { why "took $secs s"; return 1; }
  [ "$(tls_code vyasa.test /readyz)" = 200 ] || { why "/readyz $(tls_code vyasa.test /readyz)"; return 1; }
  tls_get vyasa.test /admin | grep -q '<div id="root"' || { why "/admin has no root div"; return 1; }
  local code
  code=$(jq -n '{password: env.OWNER_PASSWORD}' | api_code POST "/sites/$VYASA_ID/env/VYASA_ADMIN_PASSWORD/reveal")
  [ "$code" = 200 ] || { why "reveal: HTTP $code"; return 1; }
  # Body straight from the reveal response into the login request, over
  # stdin: the password is never on any argv.
  code=$(jq --arg email "$OWNER_EMAIL" '{email: $email, password: .value}' "$SETU_E2E_DIR/last.json" |
    docker exec -i "$CTN" curl -sk -o /dev/null -w '%{http_code}' --max-time 15 \
      --resolve vyasa.test:443:127.0.0.1 -H 'Origin: https://vyasa.test' \
      -H 'Content-Type: application/json' --data-binary @- https://vyasa.test/api/v1/auth/login)
  rm -f "$SETU_E2E_DIR/last.json"
  [ "$code" = 200 ] || { why "Vyasa login: HTTP $code"; return 1; }
}

both_serve() {
  [ "$(tls_get two.test /)" = "hello from e2e v0.1.0" ] &&
    [ "$(tls_code vyasa.test /readyz)" = 200 ]
}

t_survives_container_restart() {
  local t0=$SECONDS
  # SIGRTMIN+3 is systemd's clean shutdown (a reboot, not a power cut).
  docker restart -s SIGRTMIN+3 -t 60 "$CTN" >/dev/null || { why "docker restart failed"; return 1; }
  wait_for 60 both_serve || { why "sites not serving 60 s after restart"; return 1; }
  note "  both sites serve $((SECONDS - t0)) s after the restart"
  cx nft list table inet setu >/dev/null || { why "nft table inet setu missing after restart"; return 1; }
}

t_reconcile_repairs_lost_autosave() {
  local f=/var/lib/caddy/.config/caddy/autosave.json
  cx test -f "$f" || { why "no autosave at $f"; return 1; }
  cx rm -f "$f"
  cx systemctl restart caddy-api
  local t0=$SECONDS
  if both_serve; then note "  (routes were already back right after the restart)"; fi
  wait_for 30 both_serve || { why "routes not back within 30 s"; return 1; }
  note "  routes back after $((SECONDS - t0)) s"
}

run_test provision_under_60s t_provision_under_60s
run_test serves_over_tls t_serves_over_tls
run_test ownership_and_modes t_ownership_and_modes
run_test public_bind_refused_by_firewall t_public_bind_refused_by_firewall
run_test "caddy_tcp_admin_closed + site_user_cannot_open_admin_socket" t_caddy_admin_closed
run_test limits_and_metrics t_limits_and_metrics
run_test backup_restore t_backup_restore
run_test update_to_0_1_1 t_update_to_0_1_1
run_test rollback_restores_database t_rollback_restores_database
run_test agent_exit_mid_provision_recovers t_agent_exit_mid_provision_recovers
run_test delete_site t_delete_site
run_test vyasa_0_2_0 t_vyasa_0_2_0
run_test survives_container_restart t_survives_container_restart
run_test reconcile_repairs_lost_autosave t_reconcile_repairs_lost_autosave
