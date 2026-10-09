#!/usr/bin/env bash
# Setu end-to-end test (Task 11, spec §1/§4/§5/§9).
#
# Boots a throwaway control-plane Postgres (port 55441, container
# `setu-e2e-pg`), `setu serve` on 127.0.0.1:7010 (bound 0.0.0.0 so the
# agent container can reach it via the bridge gateway), a privileged
# `jrei/systemd-ubuntu:24.04` container (`setu-e2e`) with the agent
# installed, then provisions `apps/hello`, asserts health, limits,
# backup, update, rollback, agent-kill-mid-provision and delete.
#
# TAP output: `ok N - <name>` per assertion, `E2E PASS` at the end.
#
# Requirements: Docker with privileged containers, cgroup v2 host,
# jq, python3, openssl, curl. Every path is absolute. Cleanup (trap)
# touches ONLY the PIDs in $E2E_DIR/*.pid and the two containers above.
# The shared test Postgres on 55440 (`setu-test-pg`) is never disturbed,
# and port 5432 is never used.
set -euo pipefail

ROOT=/home/gopal/Desktop/wp/setu-work-1
E2E_DIR=/tmp/opencode/setu-e2e
mkdir -p "$E2E_DIR"

PG_NAME=setu-e2e-pg
PG_PORT=55441
CTN=setu-e2e
SERVE_PORT=7010
ART_PORT=8123
API=http://127.0.0.1:${SERVE_PORT}/api/v1
JAR=$E2E_DIR/cookies.txt
LOG=$E2E_DIR/e2e.log
N=0

OWNER_EMAIL=owner@e2e.test

# ---------------------------------------------------------------- helpers

# All script output also lands in $LOG (the trap tail uses it).
: >"$LOG"
exec > >(tee -a "$LOG") 2>&1

pass() { N=$((N + 1)); echo "ok $N - $1"; }
fail() { N=$((N + 1)); echo "not ok $N - $1"; echo "  detail: $2"; exit 1; }

need() {
  command -v "$1" >/dev/null 2>&1 || fail "prerequisite $1" "not on PATH"
}

# Wait until `cmd...` succeeds (timeout seconds). Fails the run on timeout.
wait_until() {
  local timeout=$1; shift
  local desc=$1; shift
  local deadline=$((SECONDS + timeout))
  while [ $SECONDS -lt "$deadline" ]; do
    if "$@" >/dev/null 2>&1; then return 0; fi
    sleep 3
  done
  return 1
}

# Poll GET $1 (a jobs URL) until .status == $2 (up to $3 s). Fails fast when
# the job lands on the opposite terminal state, dumping the job log.
wait_job() {
  local url=$1 want=$2 timeout=$3
  local deadline=$((SECONDS + timeout))
  local status="" body=""
  while [ $SECONDS -lt "$deadline" ]; do
    body=$(curl -s -b "$JAR" "$url") || true
    status=$(echo "$body" | jq -r '.status // empty' 2>/dev/null) || status=""
    if [ "$status" = "$want" ]; then return 0; fi
    if { [ "$want" = "completed" ] && [ "$status" = "failed" ]; } ||
       { [ "$want" = "failed" ] && [ "$status" = "completed" ]; }; then
      echo "--- job landed $status, log: ---"
      echo "$body" | jq -r '.log // empty'
      echo "--- error: $(echo "$body" | jq -r '.error // empty')"
      return 1
    fi
    sleep 3
  done
  echo "--- timed out waiting for $want, last status=$status, log: ---"
  echo "$body" | jq -r '.log // empty'
  return 1
}

# Poll GET $1 (a sites URL) until .status == $2 (up to $3 s).
wait_site() {
  local url=$1 want=$2 timeout=$3
  local deadline=$((SECONDS + timeout))
  local status=""
  while [ $SECONDS -lt "$deadline" ]; do
    status=$(curl -s -b "$JAR" "$url" | jq -r '.status // empty') || true
    if [ "$status" = "$want" ]; then return 0; fi
    sleep 3
  done
  echo "last site status: $status"
  return 1
}

cexec() { docker exec "$CTN" "$@"; }

# Best-effort agent-side forensics, run before failing on a stuck job.
# Never fails itself (|| true at call sites): the container may be gone.
dump_agent_debug() {
  echo "--- agent debug (container $CTN) ---"
  if ! docker ps --format '{{.Names}}' | grep -qx "$CTN"; then
    echo "container not running"; return 0
  fi
  echo "## systemctl status setu-agent:"
  cexec systemctl status setu-agent --no-pager 2>&1 | head -20 || true
  echo "## journal setu-agent (last 60):"
  cexec journalctl -u setu-agent -n 60 --no-pager 2>&1 | tail -60 || true
  echo "## services:"
  cexec systemctl is-active postgresql caddy setu-agent 2>&1 || true
  echo "## postgres ping:"
  cexec sudo -u postgres psql -tAc 'SELECT 1' 2>&1 | head -3 || true
  echo "## agent.toml server_id:"
  cexec grep -E 'server_id|control_plane' /etc/setu/agent.toml 2>&1 || true
  echo "--- end agent debug ---"
}

# True when the container's systemd has settled (running, or degraded:
# a fresh privileged container often idles at degraded and never reaches
# running, so requiring exactly `running` would hang forever).
systemd_settled() {
  local st
  st=$(docker exec "$CTN" systemctl is-system-running 2>/dev/null) || true
  [ "$st" = "running" ] || [ "$st" = "degraded" ]
}

# True when a REAL heartbeat landed: `join` stamps `online` itself, so
# status alone passes on join freshness. `mem_free_mb` is only ever
# written by `POST /agent/v1/heartbeat` (join leaves it NULL).
server_online() {
  local json
  json=$(curl -s -b "$JAR" "$API/servers/$SERVER_ID") || return 1
  [ "$(echo "$json" | jq -r .status 2>/dev/null)" = "online" ] &&
    [ "$(echo "$json" | jq -r .mem_free_mb 2>/dev/null)" != "null" ]
}

cleanup() {
  local rc=$?
  # Kill ONLY recorded PIDs (never by name: production services share this user).
  for pidfile in "$E2E_DIR/serve.pid" "$E2E_DIR/artifacts.pid"; do
    if [ -f "$pidfile" ]; then
      local pid; pid=$(cat "$pidfile")
      if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then kill "$pid" 2>/dev/null || true; fi
      rm -f "$pidfile"
    fi
  done
  # Remove ONLY our two containers.
  docker rm -f "$CTN" "$PG_NAME" >/dev/null 2>&1 || true
  if [ $rc -eq 0 ]; then echo "E2E PASS"; else echo "E2E FAIL (see $LOG)"; fi
  exit $rc
}
trap cleanup EXIT

# ---------------------------------------------------------------- preflight

need docker; need jq; need python3; need openssl; need curl; need cargo
[ "$(stat -fc %T /sys/fs/cgroup)" = "cgroup2fs" ] || fail "cgroup v2" "host is not cgroup v2"
[ "${SETU_TEST_PG_PORT:-55440}" != "5432" ] || fail "port guard" "refusing to run near 5432"

# Fresh state (our containers only).
docker rm -f "$CTN" "$PG_NAME" >/dev/null 2>&1 || true
rm -f "$JAR" "$E2E_DIR/serve.pid" "$E2E_DIR/artifacts.pid"

# ---------------------------------------------------------------- build
# Debug binaries: ~3x faster to build than release and small enough to
# copy into the container (Ruling in docs/E2E.md).

export CARGO_BUILD_JOBS=4
cargo build --manifest-path "$ROOT/Cargo.toml" --bin setu --bin hello 2>&1 | tail -2
SETU_BIN=$ROOT/target/debug/setu
HELLO_BIN=$ROOT/target/debug/hello
[ -x "$SETU_BIN" ] || fail "build setu" "binary missing"
[ -x "$HELLO_BIN" ] || fail "build hello" "binary missing"
pass "build setu + hello (debug)"

# glibc ceiling: the container is Ubuntu 24.04 (glibc 2.39); host-built
# gnu binaries must not need newer symbols.
MAX_GLIBC=$(objdump -T "$HELLO_BIN" | grep -oP 'GLIBC_\d+\.\d+' | sort -Vu | tail -1)
echo "  hello needs at most $MAX_GLIBC"
pass "hello glibc-compatible with container ($MAX_GLIBC)"

# ---------------------------------------------------------------- control plane postgres

docker run -d --name "$PG_NAME" \
  -e POSTGRES_USER=setu -e POSTGRES_PASSWORD=setu \
  -p 127.0.0.1:${PG_PORT}:5432 postgres:16-alpine >/dev/null
wait_until 60 "e2e postgres ready" \
  docker exec "$PG_NAME" pg_isready -U setu -d postgres \
  || fail "start control-plane postgres" "pg_isready timed out on $PG_PORT"
docker exec "$PG_NAME" psql -U setu -d postgres -c 'CREATE DATABASE setu_e2e' >/dev/null
pass "control-plane postgres on $PG_PORT (container $PG_NAME)"

export SETU_DATABASE_URL="postgres://setu:setu@127.0.0.1:${PG_PORT}/setu_e2e"
export SETU_SECRET_KEY; SETU_SECRET_KEY=$(openssl rand -hex 32)

# Bridge gateway: the agent container and artefact downloads reach the
# host through this IP (serve + artefact server bind 0.0.0.0).
GW=$(docker network inspect bridge -f '{{range .IPAM.Config}}{{.Gateway}}{{end}}' \
  | grep -oE '[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+' | head -1)
[ -n "$GW" ] || fail "bridge gateway" "empty gateway for bridge network"
export SETU_PUBLIC_URL="http://${GW}:${SERVE_PORT}"
export SETU_LISTEN="0.0.0.0:${SERVE_PORT}"
echo "  bridge gateway: $GW"

"$SETU_BIN" migrate
OWNER_PASSWORD=$(openssl rand -base64 18)
SETU_OPERATOR_PASSWORD="$OWNER_PASSWORD" "$SETU_BIN" operator create --email "$OWNER_EMAIL"
pass "migrate + owner operator created"

"$SETU_BIN" serve >"$E2E_DIR/serve.log" 2>&1 &
echo $! >"$E2E_DIR/serve.pid"
wait_until 30 "serve /healthz" curl -sf "http://127.0.0.1:${SERVE_PORT}/healthz" \
  || fail "setu serve" "no /healthz after 30s ($(tail -5 "$E2E_DIR/serve.log"))"
curl -sf "http://127.0.0.1:${SERVE_PORT}/readyz" >/dev/null \
  || fail "serve /readyz" "not ready"
pass "setu serve on :$SERVE_PORT (pid $(cat "$E2E_DIR/serve.pid"))"

# Login (session cookie jar for every later API call).
CODE=$(curl -s -o /dev/null -w '%{http_code}' -c "$JAR" -H "Origin: $SETU_PUBLIC_URL" -X POST "$API/auth/login" \
  -H 'Content-Type: application/json' \
  -d "{\"email\":\"$OWNER_EMAIL\",\"password\":\"$OWNER_PASSWORD\"}")
[ "$CODE" = "200" ] || fail "owner login" "HTTP $CODE"
curl -s -b "$JAR" "$API/auth/me" | grep -q "$OWNER_EMAIL" \
  || fail "auth/me" "owner email not returned"
pass "owner login (cookie session)"

# ---------------------------------------------------------------- artefacts

ART_DIR=$E2E_DIR/artifacts
mkdir -p "$ART_DIR"
STAGE=$E2E_DIR/stage-hello; rm -rf "$STAGE"; mkdir -p "$STAGE"
cp "$HELLO_BIN" "$STAGE/hello"
chmod 0755 "$STAGE/hello"
# One tarball per version (same bytes: versions differ in manifest
# version string + HELLO_NAME env, which is enough to observe the
# update/rollback switch; Ruling in docs/E2E.md).
for v in 0.1.0 0.1.1 0.1.2; do
  tar -czf "$ART_DIR/hello-$v.tar.gz" -C "$STAGE" hello
done
SHA_010=$(sha256sum "$ART_DIR/hello-0.1.0.tar.gz" | cut -d' ' -f1)
SHA_011=$(sha256sum "$ART_DIR/hello-0.1.1.tar.gz" | cut -d' ' -f1)
SHA_012=$(sha256sum "$ART_DIR/hello-0.1.2.tar.gz" | cut -d' ' -f1)
[ ${#SHA_010} -eq 64 ] || fail "sha256" "bad digest length"

render_manifest() { # version hello_name extra_env sha url out
  sed -e "s/__VERSION__/$1/" -e "s/__HELLO_NAME__/$2/" \
      -e "s|__EXTRA_ENV__|$3|" -e "s/__SHA256__/$4/" \
      -e "s|__URL__|$5|" "$ROOT/apps/hello/manifest.toml" >"$6"
}
render_manifest "0.1.0" "hello" "" "$SHA_010" "http://${GW}:${ART_PORT}/hello-0.1.0.tar.gz" "$E2E_DIR/hello-0.1.0.toml"
render_manifest "0.1.1" "hello-1" "" "$SHA_011" "http://${GW}:${ART_PORT}/hello-0.1.1.tar.gz" "$E2E_DIR/hello-0.1.1.toml"
render_manifest "0.1.2" "hello-1" 'HELLO_FAIL = { value = "1" }' "$SHA_012" "http://${GW}:${ART_PORT}/hello-0.1.2.tar.gz" "$E2E_DIR/hello-0.1.2.toml"

python3 -m http.server "$ART_PORT" --bind 0.0.0.0 --directory "$ART_DIR" \
  >"$E2E_DIR/artifacts.log" 2>&1 &
echo $! >"$E2E_DIR/artifacts.pid"
wait_until 20 "artefact server" curl -sf "http://${GW}:${ART_PORT}/hello-0.1.0.tar.gz" -o /dev/null \
  || fail "artefact server" "unreachable via $GW:$ART_PORT"
pass "artefacts served on :$ART_PORT (sha256 computed)"

# ---------------------------------------------------------------- agent container
#
# Host inotify budget: container-side systemd (running as host root, no
# userns remap) needs inotify instances from the HOST's
# fs.inotify.max_user_instances budget, and production load (kind,
# code-server, …) can exhaust the 128 default (systemd then dies with
# "Failed to create control group inotify object: Too many open files").
# Raising the ceiling is the documented tuning for container hosts and
# only ever adds headroom: bump to at least 8192 when lower, never lower
# it. Requires the privileged Docker the e2e needs anyway.
CUR_INOTIFY=$(cat /proc/sys/fs/inotify/max_user_instances)
if [ "$CUR_INOTIFY" -lt 8192 ]; then
  docker run --rm --privileged --entrypoint /bin/bash jrei/systemd-ubuntu:24.04 \
    -c 'sysctl -w fs.inotify.max_user_instances=8192' >/dev/null
  echo "  inotify max_user_instances: $CUR_INOTIFY → $(cat /proc/sys/fs/inotify/max_user_instances) (persist until reboot)"
fi

docker run -d -t --name "$CTN" --privileged --cgroupns=host \
  -v /sys/fs/cgroup:/sys/fs/cgroup:rw \
  --add-host=host.docker.internal:host-gateway \
  jrei/systemd-ubuntu:24.04 >/dev/null
wait_until 60 "container systemd" systemd_settled \
  || fail "systemd container" "systemd never settled"
pass "systemd container $CTN running"

# Essentials the base image lacks (sudo for the agent runtime + psql,
# curl for assertions). postgres-16 + caddy come from `agent install`.
cexec apt-get update -qq
cexec apt-get install -y -qq sudo curl ca-certificates >/dev/null
cexec mkdir -p /opt/setu
docker cp "$SETU_BIN" "$CTN:/opt/setu/setu"
cexec chmod 0755 /opt/setu/setu
cexec /opt/setu/setu agent doctor >/dev/null 2>&1 || true  # doctor fails pre-install; must not abort
pass "container essentials + setu binary at /opt/setu/setu"

# Join: create the server row, install the agent with its one-time token.
CREATE_RESP=$(curl -s -b "$JAR" -H "Origin: $SETU_PUBLIC_URL" -X POST "$API/servers" \
  -H 'Content-Type: application/json' -d '{"name":"e2e"}')
SERVER_ID=$(echo "$CREATE_RESP" | jq -r '.server.id // empty')
JOIN_TOKEN=$(echo "$CREATE_RESP" | jq -r '.join_token // empty')
[ -n "$SERVER_ID" ] && [ -n "$JOIN_TOKEN" ] || fail "POST /servers" "$CREATE_RESP"
echo "$CREATE_RESP" | jq -r '.install_command // empty' | grep -q "^setu agent install --control-plane " \
  || fail "install_command" "unexpected install command"
echo "$CREATE_RESP" | jq -r '.install_command // empty' | grep -qF "$JOIN_TOKEN" \
  && fail "install_command" "join token inside the install command"
pass "server row created ($SERVER_ID)"

# `--tls internal`: hello.test has no public DNS, so ACME would fail;
# Caddy issues from its internal CA and the checks use `curl -k`.
# The token goes in the environment (never argv); the control plane is
# plain http on the docker gateway, hence `--insecure`.
docker exec -e SETU_JOIN_TOKEN="$JOIN_TOKEN" "$CTN" /opt/setu/setu agent install \
  --control-plane "http://${GW}:${SERVE_PORT}" --insecure \
  --tls internal \
  >"$E2E_DIR/install.log" 2>&1 \
  || fail "agent install" "$(tail -15 "$E2E_DIR/install.log")"
pass "agent install (postgres-16 + caddy via apt, joined)"

wait_until 300 "server online" server_online \
  || fail "server online" "no heartbeat in 300s"
pass "agent online (heartbeat flowing)"

# ---------------------------------------------------------------- catalogue

for v in 0.1.0 0.1.1 0.1.2; do
  RESP=$(curl -s -b "$JAR" -H "Origin: $SETU_PUBLIC_URL" -X POST "$API/apps" -H 'Content-Type: application/json' \
    --data "$(jq -n --rawfile toml "$E2E_DIR/hello-$v.toml" '{toml: $toml}')")
  echo "$RESP" | jq -e '.version.version == "'"$v"'"' >/dev/null \
    || fail "publish hello $v" "$RESP"
done
pass "hello 0.1.0/0.1.1/0.1.2 published"

PLAN_ID=$(curl -s -b "$JAR" "$API/plans" | jq -r '.plans[] | select(.name == "Default") | .id')
[ -n "$PLAN_ID" ] || fail "Default plan" "not seeded"
pass "Default plan found ($PLAN_ID)"

# ---------------------------------------------------------------- provision site 1

SITE_RESP=$(curl -s -b "$JAR" -H "Origin: $SETU_PUBLIC_URL" -X POST "$API/sites" -H 'Content-Type: application/json' \
  -d '{"name":"hello","app_id":"hello","version":"0.1.0","plan_id":"'"$PLAN_ID"'","server_id":"'"$SERVER_ID"'","domain":"hello.test","owner_email":"'"$OWNER_EMAIL"'","prompts":{}}')
SITE_ID=$(echo "$SITE_RESP" | jq -r '.site.id // empty')
JOB_ID=$(echo "$SITE_RESP" | jq -r '.job.id // empty')
[ -n "$SITE_ID" ] && [ -n "$JOB_ID" ] || fail "POST /sites hello.test" "$SITE_RESP"
pass "site 1 requested ($SITE_ID)"

wait_job "$API/jobs/$JOB_ID" completed 300 \
  || { dump_agent_debug || true; fail "provision site 1" "job $JOB_ID did not complete"; }
wait_site "$API/sites/$SITE_ID" running 30 \
  || fail "site 1 running" "site never running"
pass "site 1 provisioned (running, 0.1.0)"

FROM=$(cexec curl -sk --resolve hello.test:443:127.0.0.1 https://hello.test/) || FROM="CURL-FAILED"
[ "$FROM" = "hello from hello" ] || fail "route hello.test" "got: $FROM"
pass "Caddy routes hello.test → 'hello from hello'"

MEM=$(cexec systemctl show "setu-site@${SITE_ID}.service" -p MemoryMax | cut -d= -f2) || MEM="SHOW-FAILED"
[ "$MEM" = "536870912" ] || fail "MemoryMax 512M" "got: $MEM"
pass "systemd MemoryMax=536870912 (Default plan 512M)"

SHORT1=$(echo "$SITE_ID" | tail -c 9)
cexec id "setu-site-${SHORT1}" >/dev/null \
  || fail "site user" "setu-site-${SHORT1} missing"
pass "site system user exists"

# ---------------------------------------------------------------- backup

BK_RESP=$(curl -s -b "$JAR" -H "Origin: $SETU_PUBLIC_URL" -X POST "$API/sites/$SITE_ID/backups")
BK_JOB=$(echo "$BK_RESP" | jq -r '.job.id // empty')
[ -n "$BK_JOB" ] || fail "POST backup" "$BK_RESP"
wait_job "$API/jobs/$BK_JOB" completed 180 \
  || { dump_agent_debug || true; fail "backup job" "job $BK_JOB did not complete"; }
DUMP=$(cexec sh -c "ls /var/lib/setu/backups/${SITE_ID}/*/db.dump 2>/dev/null | head -1") || DUMP=""
[ -n "$DUMP" ] || fail "backup artefacts" "no db.dump under /var/lib/setu/backups/$SITE_ID"
curl -s -b "$JAR" "$API/sites/$SITE_ID/backups" | jq -e '.backups[0].status == "completed"' >/dev/null \
  || fail "backup row" "no completed row"
pass "manual backup completed ($DUMP)"

# ---------------------------------------------------------------- update to 0.1.1

UP_RESP=$(curl -s -b "$JAR" -H "Origin: $SETU_PUBLIC_URL" -X POST "$API/sites/$SITE_ID/update" \
  -H 'Content-Type: application/json' -d '{"version":"0.1.1"}')
UP_JOB=$(echo "$UP_RESP" | jq -r '.job.id // empty')
[ -n "$UP_JOB" ] || fail "POST update 0.1.1" "$UP_RESP"
wait_job "$API/jobs/$UP_JOB" completed 300 \
  || { dump_agent_debug || true; fail "update to 0.1.1" "job $UP_JOB did not complete"; }
[ "$(curl -s -b "$JAR" "$API/sites/$SITE_ID" | jq -r .app_version)" = "0.1.1" ] \
  || fail "site version" "not 0.1.1 after update"
FROM=$(cexec curl -sk --resolve hello.test:443:127.0.0.1 https://hello.test/) || FROM="CURL-FAILED"
[ "$FROM" = "hello from hello-1" ] || fail "updated greeting" "got: $FROM"
pass "update to 0.1.1 (version + greeting observed)"

# ---------------------------------------------------------------- broken 0.1.2 → rollback

BAD_RESP=$(curl -s -b "$JAR" -H "Origin: $SETU_PUBLIC_URL" -X POST "$API/sites/$SITE_ID/update" \
  -H 'Content-Type: application/json' -d '{"version":"0.1.2"}')
BAD_JOB=$(echo "$BAD_RESP" | jq -r '.job.id // empty')
[ -n "$BAD_JOB" ] || fail "POST update 0.1.2" "$BAD_RESP"
wait_job "$API/jobs/$BAD_JOB" failed 300 \
  || { dump_agent_debug || true; fail "broken update fails" "job $BAD_JOB did not fail"; }
curl -s -b "$JAR" "$API/jobs/$BAD_JOB" | jq -r '.error // empty' | grep -q "rollback" \
  || fail "rollback recorded" "job error has no 'rollback'"
SITE_JSON=$(curl -s -b "$JAR" "$API/sites/$SITE_ID")
{ [ "$(echo "$SITE_JSON" | jq -r .status)" = "running" ] &&
  [ "$(echo "$SITE_JSON" | jq -r .app_version)" = "0.1.1" ]; } \
  || fail "rollback state" "$SITE_JSON"
FROM=$(cexec curl -sk --resolve hello.test:443:127.0.0.1 https://hello.test/) || FROM="CURL-FAILED"
[ "$FROM" = "hello from hello-1" ] || fail "post-rollback route" "got: $FROM"
pass "0.1.2 rejected → rollback to running 0.1.1"

# ---------------------------------------------------------------- kill agent mid-provision

SITE2_RESP=$(curl -s -b "$JAR" -H "Origin: $SETU_PUBLIC_URL" -X POST "$API/sites" -H 'Content-Type: application/json' \
  -d '{"name":"hello2","app_id":"hello","version":"0.1.0","plan_id":"'"$PLAN_ID"'","server_id":"'"$SERVER_ID"'","domain":"hello2.test","owner_email":"'"$OWNER_EMAIL"'","prompts":{}}')
SITE2_ID=$(echo "$SITE2_RESP" | jq -r '.site.id // empty')
JOB2_ID=$(echo "$SITE2_RESP" | jq -r '.job.id // empty')
[ -n "$SITE2_ID" ] && [ -n "$JOB2_ID" ] || fail "POST /sites hello2.test" "$SITE2_RESP"
# Wait until the provision is genuinely in flight (first step logged),
# so the kill lands mid-provision rather than before delivery.
KILL_DEADLINE=$((SECONDS + 120))
while ! curl -s -b "$JAR" "$API/jobs/$JOB2_ID" | jq -r '.log // empty' | grep -q "step .*:"; do
  [ $SECONDS -lt $KILL_DEADLINE ] || fail "provision started" "no steps logged in 120s"
  sleep 3
done
cexec systemctl kill setu-agent
sleep 5
cexec systemctl start setu-agent
wait_job "$API/jobs/$JOB2_ID" completed 420 \
  || { dump_agent_debug || true; fail "provision survives agent kill" "job $JOB2_ID did not complete"; }
wait_site "$API/sites/$SITE2_ID" running 30 \
  || fail "site 2 running" "site never running after agent restart"
FROM=$(cexec curl -sk --resolve hello2.test:443:127.0.0.1 https://hello2.test/) || FROM="CURL-FAILED"
[ "$FROM" = "hello from hello" ] || fail "route hello2.test" "got: $FROM"
pass "agent kill mid-provision → redelivery → site 2 running"

# ---------------------------------------------------------------- delete site 1

DEL_RESP=$(curl -s -b "$JAR" -H "Origin: $SETU_PUBLIC_URL" -X DELETE "$API/sites/$SITE_ID")
DEL_JOB=$(echo "$DEL_RESP" | jq -r '.job.id // empty')
[ -n "$DEL_JOB" ] || fail "DELETE site 1" "$DEL_RESP"
# The delete job removes the site row, cascading its own job row: the
# job URL 404s on success (same contract the API test asserts), so poll
# until `completed`, `failed`, or gone — gone means done.
DEL_DONE="" DEL_STATUS="" DEL_CODE=""
DEL_DEADLINE=$((SECONDS + 180))
while [ $SECONDS -lt $DEL_DEADLINE ]; do
  DEL_RESP2=$(curl -s -b "$JAR" -o "$E2E_DIR/del-job.json" -w '%{http_code}' "$API/jobs/$DEL_JOB") || DEL_RESP2="000"
  DEL_CODE="$DEL_RESP2"
  DEL_STATUS=$(jq -r '.status // empty' "$E2E_DIR/del-job.json" 2>/dev/null) || DEL_STATUS=""
  if [ "$DEL_STATUS" = "completed" ] || [ "$DEL_CODE" = "404" ]; then DEL_DONE=1; break; fi
  if [ "$DEL_STATUS" = "failed" ]; then break; fi
  sleep 3
done
[ -n "$DEL_DONE" ] \
  || { dump_agent_debug || true; fail "delete job" "job $DEL_JOB status=$DEL_STATUS code=$DEL_CODE"; }
CODE=$(curl -s -o /dev/null -w '%{http_code}' -b "$JAR" "$API/sites/$SITE_ID")
[ "$CODE" = "404" ] || fail "site row gone" "GET still $CODE"
cexec sh -c "test ! -d /srv/setu/sites/${SITE_ID}" \
  || fail "site dir gone" "directory remains"
cexec sh -c "test ! -d /etc/systemd/system/setu-site@${SITE_ID}.service.d" \
  || fail "drop-in gone" "drop-in remains"
if cexec id "setu-site-${SHORT1}" >/dev/null 2>&1; then
  fail "site user gone" "user remains"
fi
# Site short id (last 8 chars, lowercase ULID); SHORT1 set above.
DB_LEFT=$(cexec sudo -u postgres psql -tAc \
  "SELECT 1 FROM pg_database WHERE datname = 'site_${SHORT1}'" | head -1) || DB_LEFT=""
[ -z "$DB_LEFT" ] || fail "site database gone" "row remains: $DB_LEFT"
pass "site 1 deleted (row, unit, db, dir, user gone)"
