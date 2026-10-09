---
name: setu-e2e
description: Running and debugging the end-to-end test (control plane + agent in a systemd container + hello app) and the installer test. Use when a task touches the agent, provisioning, or install.sh.
---

# End-to-end

`SETU_E2E_DIR="$SCRATCH/e2e" bash /home/gopal/Desktop/wp/setu/scripts/e2e.sh > "$SCRATCH/e2e.log" 2>&1; tail -50 "$SCRATCH/e2e.log"`

`$SCRATCH` is this session's scratchpad directory. The script writes
PIDs (`serve.pid`, `httpd.pid`) and container names into `SETU_E2E_DIR`.

What it does: builds the binaries, starts a control-plane Postgres
(port 55441) and `setu serve` on 127.0.0.1:7010, starts
`jrei/systemd-ubuntu:24.04` as `setu-e2e` with `--privileged
--cgroupns=private` (NEVER bind-mount the host's /sys/fs/cgroup and
NEVER `--network host`: production services run on this host), installs
the agent with `--tls internal --insecure`, joins, provisions
`apps/hello`, asserts TLS, firewall, Caddy admin closed, limits,
metrics, backup/restore, update, rollback with database restore,
deterministic agent exit mid-provision, delete, real Vyasa 0.2.0, and
survival of a container restart. TAP output, 12 assertions.

Debugging: `docker exec setu-e2e systemctl status setu-agent
'setu-site@*'`; `docker exec setu-e2e journalctl -u setu-agent -n 200`;
`docker exec setu-e2e cat /var/lib/setu/state.json`; `docker exec
setu-e2e curl -s localhost:2019/config/`; control-plane job log via
`GET /api/v1/jobs/<id>`. Fix the code, never the assertion.

Cleanup is in the script's trap; if it died hard: `docker rm -f
setu-e2e setu-e2e-pg` and kill only the PIDs in `$SETU_E2E_DIR/*.pid`.
Requirements: Docker with privileged containers, cgroup v2 host, the
`x86_64-unknown-linux-musl` target installed.
