---
name: setu-e2e
description: Running and debugging the end-to-end test (control plane + agent in a systemd container + hello app) and the installer test. Use when a task touches the agent, provisioning, or install.sh.
---

# End-to-end

`SETU_E2E_DIR="$SCRATCH/e2e" bash /home/gopal/Desktop/wp/setu/scripts/e2e.sh > "$SCRATCH/e2e.log" 2>&1; tail -50 "$SCRATCH/e2e.log"`

`$SCRATCH` is this session's scratchpad directory. The script writes
PIDs (`serve.pid`, `httpd.pid`) and container names into `SETU_E2E_DIR`.

What it does: builds `setu` (musl release, features `test-root,e2e-hooks`;
clang stands in for `musl-gcc` when it is missing) and `hello`, creates
Docker network `setu-e2e-net`, starts a control-plane Postgres on it
and `setu serve` on the host's address on that network (port 7010),
creates the owner with the setup token (every mutation sends `Origin`),
starts the image `scripts/e2e/Dockerfile` (`jrei/systemd-ubuntu:24.04`
with the `e2e-init` cgroup2 entrypoint) as `setu-e2e` with
`--privileged --cgroupns=private` (NEVER bind-mount the host's
/sys/fs/cgroup and NEVER `--network host`: production services run on
this host), installs the agent with `--tls internal --insecure` and
`SETU_JOIN_TOKEN` in the environment, then runs 14 TAP assertions:
`provision_under_60s`, `serves_over_tls`, `ownership_and_modes`,
`public_bind_refused_by_firewall`, `caddy_tcp_admin_closed +
site_user_cannot_open_admin_socket`, `limits_and_metrics`,
`backup_restore`, `update_to_0_1_1`, `rollback_restores_database`,
`agent_exit_mid_provision_recovers`, `delete_site`, `vyasa_0_2_0`,
`survives_container_restart`, `reconcile_repairs_lost_autosave`; then
`E2E PASS`. Setup steps are `# …` comments (`Bail out!` on failure);
each `not ok` writes `$SETU_E2E_DIR/diag-NN-<name>.txt`. Full table:
`docs/E2E.md`.

`scripts/test-install.sh` is the sibling for `install.sh`
(`SETU_INSTALL_DIR`, network `setu-install-net`, release build without
features): 9 assertions `install_sh_succeeds`, `healthz_through_caddy`,
`setup_without_token_401`, `setup_with_token_201`, `login`,
`local_server_joins`, `agent_doctor`, `self_backup`, `rerun_is_noop`,
then `INSTALL PASS`.

Debugging: `docker exec setu-e2e systemctl status setu-agent
'setu-site@*'`; `docker exec setu-e2e journalctl -u setu-agent -n 200`;
`docker exec setu-e2e cat /var/lib/setu/state.json`; `docker exec
setu-e2e curl -s --unix-socket /run/caddy/admin.sock http://localhost/config/`;
`docker exec setu-e2e /opt/setu/setu agent doctor`; control-plane job log via
`GET /api/v1/jobs/<id>` (cookie jar `$SETU_E2E_DIR/cookies.txt`).
`SETU_E2E_KEEP=1` leaves the containers up. Site units are started with
`enable --now` and stopped with `disable --now` (they survive the
container restart). Fix the code, never the assertion.

Cleanup is in the script's trap; if it died hard: `docker rm -f
setu-e2e setu-e2e-pg` and kill only the PIDs in `$SETU_E2E_DIR/*.pid`.
Requirements: Docker with privileged containers, cgroup v2 host, the
`x86_64-unknown-linux-musl` target installed, a C compiler for it
(`musl-tools` or clang), outbound HTTPS from the box.
