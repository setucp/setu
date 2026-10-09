---
name: setu-e2e
description: Running and debugging the end-to-end test (control plane + agent in a systemd container + hello app) and the installer test. Use when a task touches the agent, provisioning, or install.sh.
---

# End-to-end

`bash /home/gopal/Desktop/wp/setu/scripts/e2e.sh > "$SCRATCH/e2e.log" 2>&1; tail -50 "$SCRATCH/e2e.log"`

What it does: builds the binaries, starts a control-plane Postgres
(port 55441) and `setu serve` on 127.0.0.1:7010, starts a privileged
`jrei/systemd-ubuntu:24.04` container named `setu-e2e`, installs the
agent, joins, provisions `apps/hello`, asserts health, limits, backup,
update, rollback, agent-kill-mid-provision, delete. TAP output.

Debugging: `docker exec setu-e2e systemctl status setu-agent
'setu-site@*'`; `docker exec setu-e2e journalctl -u setu-agent -n 200`;
`docker exec setu-e2e cat /var/lib/setu/state.json`; `docker exec
setu-e2e curl -s localhost:2019/config/`; control-plane job log via
`GET /api/v1/jobs/<id>`. Fix the code, never the assertion.

Cleanup is in the script's trap; if it died hard: `docker rm -f
setu-e2e setu-e2e-pg` and kill the PID in `$SCRATCH/e2e-serve.pid`
only. Requirements: Docker with privileged containers; cgroup v2 host.
