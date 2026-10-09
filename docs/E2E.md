# End-to-end test

`bash /home/gopal/Desktop/wp/setu-work-1/scripts/e2e.sh` — boots a
throwaway control-plane Postgres (port 55441, container `setu-e2e-pg`),
`setu serve` on `:7010`, and a privileged `jrei/systemd-ubuntu:24.04`
container (`setu-e2e`) with the agent installed, then provisions
`apps/hello` and asserts the whole lifecycle. TAP output (`ok N - name`),
`E2E PASS` at the end. Takes 15–40 minutes (debug build + apt inside the
container + provision/update/rollback/delete with real timeouts).

## What it asserts

| # | Assertion |
|---|---|
| 1 | `setu` + `hello` build (debug); hello needs at most glibc 2.34 (runs on the container's 2.39) |
| 2 | Control-plane Postgres up on 55441; migrate + owner operator |
| 3 | `setu serve` answers `/healthz` + `/readyz`; owner login works |
| 4 | Hello artefacts served over HTTP with computed sha256 |
| 5 | Privileged systemd container settles; `agent install` joins |
| 6 | Server `online` via heartbeat |
| 7 | hello 0.1.0/0.1.1/0.1.2 published; Default plan present |
| 8 | Provision → `running`; `curl -H 'Host: hello.test'` → `hello from hello` |
| 9 | `MemoryMax=536870912` on the site unit (Default plan 512M); site user exists |
| 10 | Manual backup completes; `db.dump` exists in the container |
| 11 | Update to 0.1.1 → `running`, version + greeting (`hello-1`) observed |
| 12 | Update to broken 0.1.2 fails with `rollback`; site stays `running` on 0.1.1 |
| 13 | Agent killed mid-provision → command redelivered → site 2 `running` |
| 14 | Delete → row 404, unit/drop-in/db/dir/user gone |

## Rulings

- **Debug, not release.** The script builds `target/debug/{setu,hello}`:
  ~3x faster than release and small enough to `docker cp`. Cost if
  wrong: none for correctness (same code paths); CI pays minutes back.
- **Same bytes for 0.1.0/0.1.1/0.1.2.** The tarballs are identical; the
  versions differ in manifest `version` + `HELLO_NAME` env value, which
  is enough to observe the switch and the rollback. Cost if wrong: a
  byte-identical update still exercises symlink switch + restart +
  health, so nothing is lost.
- **Direct `docker run`, not the Dockerfile.** `scripts/e2e/Dockerfile`
  only pre-bakes `sudo curl ca-certificates`; the script installs them
  with apt after boot (seconds) so there is one path, not two.
  `E2E_IMAGE` is not a script knob: edit the `docker run` line to use a
  prebuilt image.
- **`agent install --tls internal` (e2e).** Test domains have no public
  DNS, so ACME issuance would fail; Caddy issues from its internal CA
  and the route checks use `curl -k --resolve <domain>:443:127.0.0.1`.
  Production default keeps ACME on.
- **Site users take the 8-char short id.** `setu-site-<full ULID>` is 36
  chars and shadow `useradd` rejects names over 32 (`invalid user name`,
  caught by the e2e at `WriteUnit`). Jobs builds `user` from
  `short_id()` — the same uniqueness basis the plan already mandates
  for db names (`site_<short>`); units keep the full id.
- **Host inotify budget raised, never lowered.** Container-side systemd
  runs as host root (no userns remap), so it draws inotify instances
  from the host's `fs.inotify.max_user_instances` (default 128), which
  production load on this host exhausts (kind, code-server, …). The
  script bumps it to 8192 when lower via a throwaway privileged
  container (`sysctl -w`; privileged Docker is already required). This
  only adds headroom, persists until reboot, and is the standard tuning
  for container hosts. Diagnosis evidence: `systemd` as PID 1 exited
  255 with `Failed to create control group inotify object: Too many
  open files` (visible only with `docker run -t`, since PID 1 logs to
  `/dev/console`); a probe binary confirmed 0 instances creatable as
  container root vs 58 as uid 1000. The script also passes `-t` so
  `docker logs setu-e2e` shows the boot for debugging.

## Debugging

- Full log: `/tmp/opencode/setu-e2e/e2e.log`; per-step logs beside it
  (`serve.log`, `install.log`, `artifacts.log`).
- While it runs: `docker exec setu-e2e systemctl status setu-agent 'setu-site@*'`;
  `docker exec setu-e2e journalctl -u setu-agent -n 200`;
  `docker exec setu-e2e cat /var/lib/setu/state.json`;
  `docker exec setu-e2e curl -s --unix-socket /run/caddy/admin.sock -H 'Host: 127.0.0.1' http://x/config/`;
  control-plane job log via `GET /api/v1/jobs/<id>` (cookie from login).
- Cleanup is the script's trap (recorded PIDs + the two containers). If
  it died hard: `docker rm -f setu-e2e setu-e2e-pg` and kill the PID in
  `/tmp/opencode/setu-e2e/serve.pid` (and `artifacts.pid`) — nothing else.
- Never touch `setu-test-pg` (port 55440) or host Postgres (5432).

## CI

The `e2e` job in `.github/workflows/ci.yml` runs the script on
`ubuntu-latest` (privileged Docker is available on GitHub-hosted
runners), gated to Rust/script/image changes, timeout 40 minutes.
