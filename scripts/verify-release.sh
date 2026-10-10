#!/usr/bin/env bash
# Verify a published release (0.1.1 acceptance): a fresh systemd
# container installs Setu from github.com/setucp/setu/releases v<ver>,
# creates the owner, joins the local agent, publishes the Miniflux and
# hello-axum manifests from github.com/setucp/manifests and provisions
# both. TAP, 9 assertions, then `VERIFY PASS` or `VERIFY FAIL`.
#
#   SETU_VERIFY_DIR=<dir> bash scripts/verify-release.sh <version>
#
# Host safety as scripts/test-install.sh: own network, no host network,
# no host cgroup mount; cleanup removes only this container and network.
# Needs outbound HTTPS from the container. SETU_VERIFY_KEEP=1 keeps it.
# SETU_TEST_IMAGE selects the systemd base image (default ubuntu:24.04, the
# multi-arch image; 0.1.2 I2 also runs debian:12 and debian:13).
set -euo pipefail

[ $# -eq 1 ] || { echo "usage: $0 <version>" >&2; exit 2; }
V=$1
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
SETU_VERIFY_DIR=${SETU_VERIFY_DIR:-${TMPDIR:-/tmp}/setu-verify}
mkdir -p "$SETU_VERIFY_DIR"
NET=setu-verify-net
CTN=setu-verify
BASE_IMAGE=${SETU_TEST_IMAGE:-ubuntu:24.04}
IMAGE="setu-verify-test-image:$(printf '%s' "$BASE_IMAGE" | tr -c 'A-Za-z0-9._-' '_')"
HOST=setu.test
ORIGIN=https://$HOST
MANIFESTS=https://raw.githubusercontent.com/setucp/manifests/main
OWNER_EMAIL=owner@verify.test
OWNER_PASSWORD=verify-owner-pass-1
TOTAL=9
N=0; FAILED=0; WHY=""

note() { echo "# $*"; }
why() { WHY="$*"; }
bail() { echo "Bail out! $*"; exit 1; }
cx() { docker exec "$CTN" "$@"; }
teardown() {
  docker rm -fv "$CTN" >/dev/null 2>&1 || true
  docker network rm "$NET" >/dev/null 2>&1 || true
}
on_exit() {
  local rc=$?
  [ "${SETU_VERIFY_KEEP:-0}" = 1 ] && note "SETU_VERIFY_KEEP=1: leaving $CTN" || teardown
  if [ $rc -eq 0 ] && [ "$FAILED" -eq 0 ] && [ "$N" -eq "$TOTAL" ]; then echo "VERIFY PASS"
  else echo "VERIFY FAIL ($FAILED failed, $N of $TOTAL run)"; [ $rc -ne 0 ] || rc=1; fi
  exit $rc
}
trap on_exit EXIT
run_test() {
  local name=$1 fn=$2; N=$((N + 1)); WHY=""
  if "$fn"; then echo "ok $N - $name"; else echo "not ok $N - $name"; echo "#   reason: ${WHY:-unknown}"; FAILED=$((FAILED + 1)); fi
}
# capi METHOD PATH [json on stdin]: prints the HTTP code; body in /root/last.json.
capi() {
  local method=$1 path=$2
  if [ "$method" = GET ]; then
    cx curl -sS -k --max-time 60 --resolve "$HOST:443:127.0.0.1" -b /root/jar -c /root/jar \
      -o /root/last.json -w '%{http_code}' "$ORIGIN/api/v1$path" || true
  else
    docker exec -i "$CTN" curl -sS -k --max-time 60 --resolve "$HOST:443:127.0.0.1" -b /root/jar -c /root/jar \
      -H "Origin: $ORIGIN" -H 'content-type: application/json' -X "$method" --data-binary @- \
      -o /root/last.json -w '%{http_code}' "$ORIGIN/api/v1$path" || true
  fi
}
last() { cx cat /root/last.json; }
poll_job() { # poll_job JOB_ID SECONDS -> prints final status
  local j=$1 deadline=$((SECONDS + $2)) st=""
  while [ $SECONDS -lt $deadline ]; do
    capi GET "/jobs/$j" >/dev/null; st=$(last | jq -r '.status // empty')
    case "$st" in done|failed) echo "$st"; return 0 ;; esac
    sleep 3
  done
  echo "${st:-timeout}"
}

echo "1..$TOTAL"
for t in docker jq curl; do command -v "$t" >/dev/null || bail "missing $t"; done
teardown
docker build -q --build-arg "BASE=$BASE_IMAGE" -t "$IMAGE" "$ROOT/scripts/e2e" >/dev/null ||
  bail "image build failed"
docker network create "$NET" >/dev/null
docker run -d --name "$CTN" --hostname setu-verify --privileged --cgroupns=private --network "$NET" \
  --add-host "$HOST:127.0.0.1" --add-host miniflux.test:127.0.0.1 --add-host hello.test:127.0.0.1 \
  "$IMAGE" >/dev/null
for _ in $(seq 90); do cx systemctl is-system-running 2>/dev/null | grep -qE 'running|degraded' && break; sleep 1; done

SERVER_ID=""; PLAN_ID=""; MINIFLUX_ID=""

t_install_from_public_release() {
  cx bash -c "curl -fsSL -o /root/install.sh https://github.com/setucp/setu/releases/download/v$V/install.sh" ||
    { why "install.sh not downloadable for v$V"; return 1; }
  timeout 1500 docker exec "$CTN" bash /root/install.sh --tls internal --panel-host "$HOST" --version "$V" \
    >"$SETU_VERIFY_DIR/install.log" 2>&1 || { why "install.sh failed: $(tail -3 "$SETU_VERIFY_DIR/install.log" | tr '\n' ' ')"; return 1; }
}
t_version_matches() {
  [ "$(cx /opt/setu/setu --version)" = "setu $V" ] || { why "binary reports $(cx /opt/setu/setu --version)"; return 1; }
}
t_owner_setup_and_login() {
  local code
  code=$(SETUP_TOKEN=$(cx cat /var/lib/setu/setup-token) jq -n --arg e "$OWNER_EMAIL" --arg p "$OWNER_PASSWORD" \
    '{token: env.SETUP_TOKEN, email: $e, password: $p}' | capi POST /setup)
  [ "$code" = 201 ] || { why "setup: HTTP $code $(last)"; return 1; }
  code=$(jq -n --arg e "$OWNER_EMAIL" --arg p "$OWNER_PASSWORD" '{email: $e, password: $p}' | capi POST /auth/login)
  [ "$code" = 200 ] || { why "login: HTTP $code"; return 1; }
}
t_local_server_joins() {
  local code cmd token
  code=$(echo '{"name":"panel"}' | capi POST /setup/local-server)
  [ "$code" = 201 ] || { why "local-server: HTTP $code $(last)"; return 1; }
  cmd=$(last | jq -r .install_command); token=$(last | jq -r .join_token); SERVER_ID=$(last | jq -r .server.id)
  docker exec -e SETU_JOIN_TOKEN="$token" "$CTN" bash -c "$cmd" >"$SETU_VERIFY_DIR/agent-install.log" 2>&1 ||
    { why "agent install failed"; return 1; }
  for _ in $(seq 60); do
    capi GET "/servers/$SERVER_ID" >/dev/null
    [ "$(last | jq -r .status)" = online ] && return 0; sleep 2
  done
  why "server never online"; return 1
}
publish_from_manifests() { # publish_from_manifests <dir>
  local code
  code=$(jq -n --arg u "$MANIFESTS/$1/manifest.toml" '{url: $u}' | capi POST /apps)
  [ "$code" = 201 ] || [ "$code" = 200 ] || { why "publish $1: HTTP $code $(last)"; return 1; }
}
create_site() { # create_site <app> <version> <domain> -> sets SITE_ID, JOB_ID
  local code
  [ -n "$PLAN_ID" ] || { capi GET /plans >/dev/null; PLAN_ID=$(last | jq -r '.plans[] | select(.name == "Default") | .id' | head -1); }
  code=$(jq -n --arg app "$1" --arg v "$2" --arg d "$3" --arg plan "$PLAN_ID" --arg srv "$SERVER_ID" --arg o "$OWNER_EMAIL" \
    '{name: $app, app_id: $app, version: $v, plan_id: $plan, server_id: $srv, domain: $d, owner_email: $o}' | capi POST /sites)
  [ "$code" = 202 ] || [ "$code" = 201 ] || { why "create $1: HTTP $code $(last)"; return 1; }
  SITE_ID=$(last | jq -r '.site.id'); JOB_ID=$(last | jq -r '.job.id')
}
t_miniflux_publishes_from_manifests_repo() { publish_from_manifests miniflux; }
t_miniflux_provisions_and_serves() {
  local v st
  v=$(curl -fsSL "$MANIFESTS/miniflux/manifest.toml" | sed -n 's/^version = "\(.*\)"/\1/p' | head -1)
  create_site miniflux "$v" miniflux.test || return 1
  MINIFLUX_ID=$SITE_ID
  st=$(poll_job "$JOB_ID" 420); [ "$st" = done ] || { why "provision job $st: $(last | jq -r .error)"; return 1; }
  cx curl -sk --max-time 20 --resolve miniflux.test:443:127.0.0.1 https://miniflux.test/healthcheck | grep -q OK ||
    { why "healthcheck did not answer OK"; return 1; }
}
t_site_detail_admin_url_prompts_and_backup_status() {
  local v
  capi GET "/sites/$MINIFLUX_ID" >/dev/null
  [ "$(last | jq -r .admin_url)" = "https://miniflux.test/" ] || { why "admin_url: $(last | jq -c .admin_url)"; return 1; }
  v=$(last | jq -r .app_version)
  [ "$(capi GET "/apps/miniflux/versions/$v/prompts")" = 200 ] || { why "prompts endpoint"; return 1; }
  [ "$(last | jq -c .prompts)" = "[]" ] || { why "unexpected prompts $(last)"; return 1; }
  [ "$(capi GET /panel/backup)" = 200 ] || { why "panel backup endpoint"; return 1; }
}
# True only for a whole number above zero: a missing site or metric makes
# jq print null or nothing, which must read as "no sample yet", not an error.
usage_is_positive() {
  case "$1" in ''|null|*[!0-9]*) return 1 ;; esac
  [ "$1" -gt 0 ]
}
t_sites_list_shows_live_usage() {
  local mem
  for _ in $(seq 60); do
    capi GET /sites >/dev/null
    mem=$(last | jq -r --arg id "$MINIFLUX_ID" '[.sites[] | select(.id == $id) | .metrics.memory_bytes // 0][0] // 0')
    usage_is_positive "$mem" && return 0
    sleep 5
  done
  why "no metric sample in the sites list after 5 minutes"; return 1
}
t_hello_axum_provisions() {
  local v health st
  publish_from_manifests hello-axum || return 1
  v=$(curl -fsSL "$MANIFESTS/hello-axum/manifest.toml" | sed -n 's/^version = "\(.*\)"/\1/p' | head -1)
  health=$(curl -fsSL "$MANIFESTS/hello-axum/manifest.toml" | sed -n 's/^health = "\(.*\)"/\1/p' | head -1)
  create_site hello-axum "$v" hello.test || return 1
  st=$(poll_job "$JOB_ID" 420); [ "$st" = done ] || { why "provision job $st"; return 1; }
  cx curl -sfk --max-time 20 --resolve hello.test:443:127.0.0.1 "https://hello.test$health" >/dev/null ||
    { why "hello.test$health not 2xx"; return 1; }
}

run_test install_from_public_release t_install_from_public_release
run_test version_matches t_version_matches
run_test owner_setup_and_login t_owner_setup_and_login
run_test local_server_joins t_local_server_joins
run_test miniflux_publishes_from_manifests_repo t_miniflux_publishes_from_manifests_repo
run_test miniflux_provisions_and_serves t_miniflux_provisions_and_serves
run_test site_detail_admin_url_prompts_and_backup_status t_site_detail_admin_url_prompts_and_backup_status
run_test sites_list_shows_live_usage t_sites_list_shows_live_usage
run_test hello_axum_provisions t_hello_axum_provisions
