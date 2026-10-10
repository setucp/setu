# End-to-end test

```
SETU_E2E_DIR=<dir> bash scripts/e2e.sh > <dir>/../e2e.log 2>&1; tail -60 <dir>/../e2e.log
```

Plan Task 11, spec §9. Builds the x86_64 musl `setu` (features
`test-root,e2e-hooks`, which the release workflow never enables) and
`apps/hello`, runs the control plane on the host and the agent in a
systemd container, provisions `hello` and real Vyasa 0.2.0, and prints
TAP: `1..14`, `ok N - name` / `not ok N - name` per assertion, then
`E2E PASS` or `E2E FAIL`. About 6 minutes with a warm cargo cache
(release build of the two binaries ~4 minutes cold).

## Topology and host safety

- Docker network `setu-e2e-net`, created and removed by the script.
  `$HOSTIP` is the host's address on it (the bridge gateway, from `docker
  network inspect`). Everything the host runs binds `$HOSTIP` only:
  `setu serve` on `:7010` (`SETU_LISTEN`, `SETU_PUBLIC_URL=http://$HOSTIP:7010`),
  the artefact server (`python3 -m http.server`) on `:8123`, and the
  control-plane Postgres `setu-e2e-pg` (postgres:16-alpine) published on
  `$HOSTIP:55441`. Nothing binds `0.0.0.0` on the host and port 5432 is
  never used.
- The agent box `setu-e2e` is the image `scripts/e2e/Dockerfile`
  (`jrei/systemd-ubuntu:24.04` + `curl ca-certificates sudo python3
  iproute2`; no Caddy, Postgres or nftables: `setu agent install`
  installs them) run with `--privileged --cgroupns=private --network
  setu-e2e-net`: no host network, no host cgroup bind mount.
- Cleanup (trap) stops only what the script started: the PIDs it saved
  in `$SETU_E2E_DIR/serve.pid` and `httpd.pid` (and only when the
  process's command line still matches), the containers `setu-e2e` and
  `setu-e2e-pg` (with their anonymous volumes), and the network. Never
  anything by name. `SETU_E2E_KEEP=1` leaves them for debugging; the
  next run removes them first.
- Secrets never reach argv or the log: the setup token, owner password
  and join token travel through the environment (`docker exec -e
  SETU_JOIN_TOKEN` without a value) or stdin (`curl --data-binary @-`),
  and the revealed Vyasa password is piped from the reveal response
  straight into the login request.

## Setup (not numbered)

Build; image; network; Postgres; `setu serve` with `SETU_DATA_DIR=
$SETU_E2E_DIR/data`; `POST /api/v1/setup` with the token from
`$SETU_E2E_DIR/data/setup-token` (every mutation carries `Origin:
$SETU_PUBLIC_URL`, ledger R103); login (cookie jar); `POST /servers`
(the install command must not contain the join token, R106); box up;
`docker cp` the binary to `/opt/setu/setu` (root, 0755); `setu agent
install --control-plane http://$HOSTIP:7010 --insecure --tls internal`
with `SETU_JOIN_TOKEN` in the environment; a drop-in
`SETU_AGENT_RECONCILE_SECS=10` for the agent; server `online` with a
real heartbeat; `setu agent doctor` exits 0; three hello archives
(same binary plus a `VERSION` file) and manifests rendered from
`apps/hello/manifest.tmpl.toml`; `POST /apps` ×3. A setup failure prints
`Bail out!` and diagnostics.

## Assertions

| # | Name | Checks |
|---|---|---|
| 1 | `provision_under_60s` | hello 0.1.0 on `hello.test`: job `done`, site `running`, elapsed < 60 s (printed) |
| 2 | `serves_over_tls` | `curl -sk --resolve hello.test:443:127.0.0.1 https://hello.test/` = `hello from e2e v0.1.0` |
| 3 | `ownership_and_modes` | `env` `root:root 600`, `data` `setu-site-<short> 750`, `bin` `root:root 755` |
| 4 | `public_bind_refused_by_firewall` | a `0.0.0.0:25000` listener in the box answers on its loopback but not from the host (`$CTN_IP:25000`); `nft list table inet setu` ok |
| 5 | `caddy_tcp_admin_closed + site_user_cannot_open_admin_socket` | `localhost:2019` closed; root can use `/run/caddy/admin.sock`, `setu-site-<short>` cannot |
| 6 | `limits_and_metrics` | `MemoryMax=536870912`; within 90 s `GET /sites/{id}` has `metrics.memory_bytes > 0` and `disk_bytes > 0` |
| 7 | `backup_restore` | note `a`; backup `done`; note `b`; restore `done` → `GET /notes` = `["a"]` and `GET /uploads` = `["note-1.txt"]` (database and `uploads` dir restored) |
| 8 | `update_to_0_1_1` | job `done`, `running`, `app_version` 0.1.1, `/` says `v0.1.1`, `notes_v2` in `\dt` |
| 9 | `rollback_restores_database` | update to 0.1.2 (`HELLO_FAIL=1`): job `failed` with `rollback: …`; `running` on 0.1.1; `notes_v3` absent, `notes_v2` present; notes `["a"]`; `/` says `v0.1.1` |
| 10 | `agent_exit_mid_provision_recovers` | drop-in `SETU_AGENT_TEST_EXIT_AFTER=write_unit` + `Restart=no`; create `two.test`; agent unit goes `inactive`; drop-in removed, `daemon-reload`, restart; job `done`, site `running`, exactly one `step write_unit:` line in the job log, and the agent journal shows `posting the cached result` (or `duplicate command: returning the cached result`) for the `write_unit` command id |
| 11 | `delete_site` | site 404; unit inactive, drop-in gone, `systemctl status` fails; user, database `site_<short>` and `/srv/setu/sites/<short>` gone; `GET /backups` lists the `pre_delete` backup with domain `hello.test`; `DELETE /backups/{bid}` → job `done`, `/var/lib/setu/backups/<short>/<bid>` gone, no longer listed |
| 12 | `vyasa_0_2_0` | `POST /apps` with `crates/manifest/tests/corpus/vyasa_0_2_0.toml` (archive from GitHub, fetched by the agent in the box); `vyasa.test` `running` in < 60 s; `/readyz` 200; `/admin` contains `<div id="root"`; reveal `VYASA_ADMIN_PASSWORD`; `POST https://vyasa.test/api/v1/auth/login {email, password}` with `Origin: https://vyasa.test` → 200 |
| 13 | `survives_container_restart` | `docker restart -s SIGRTMIN+3` (systemd's clean shutdown): `two.test` and `vyasa.test` serve over TLS within 60 s; `nft list table inet setu` ok |
| 14 | `reconcile_repairs_lost_autosave` | `rm /var/lib/caddy/.config/caddy/autosave.json`; `systemctl restart caddy-api`; both routes back within 30 s through reconcile |

## `apps/hello`

One binary; the version and failure flag come from the manifest's `env`
values. `GET /` → `hello from <HELLO_NAME> v<HELLO_VERSION>`;
`/healthz` 500 when `HELLO_FAIL=1`, else 200 after `SELECT 1`;
`GET|POST /notes` (table `notes`), each note also written to
`HELLO_UPLOADS_DIR` (`uploads`, backed up) and listed by `GET /uploads`;
a `last-boot` marker in `HELLO_STATE_DIR` (`state`, not backed up).
Boot migrations: 0.1.1 creates `notes_v2` and refuses to boot when
`notes_v3` exists; 0.1.2 creates `notes_v3` and serves (health fails).
So a rollback that does not replace the database is visibly broken.

## Requirements

Docker with privileged containers on a cgroup v2 host; `jq curl openssl
python3`; the `x86_64-unknown-linux-musl` target (in
`rust-toolchain.toml`) and a C compiler for it (`musl-tools`; without
`musl-gcc` the script sets `CC_x86_64_unknown_linux_musl=clang`, which
builds `ring` and `zstd-sys` against the system headers, ledger R119);
outbound HTTPS from the box (Ubuntu archive, Caddy, GitHub).

## Debugging

`SETU_E2E_KEEP=1`, then: `docker exec setu-e2e systemctl status
setu-agent caddy-api 'setu-site@*'`; `docker exec setu-e2e journalctl
-u setu-agent -n 200`; `docker exec setu-e2e cat
/var/lib/setu/state.json`; `docker exec setu-e2e curl -s --unix-socket
/run/caddy/admin.sock http://localhost/config/`; `GET
/api/v1/jobs/<id>` with the cookie jar `$SETU_E2E_DIR/cookies.txt`.
Each `not ok` writes `$SETU_E2E_DIR/diag-NN-<name>.txt` (serve log,
unit status, agent and site journals, state, Caddy routes, the job
logs) and prints its tail. Fix the code, never the assertion.

## CI

Job `e2e` in `.github/workflows/ci.yml` on `ubuntu-24.04` with the musl
target and `musl-tools`; uploads the log and diagnostics on failure.

## The installer test

`SETU_INSTALL_DIR=<dir> bash scripts/test-install.sh > <log> 2>&1`
(TAP, 9 assertions, then `INSTALL PASS`). Same host-safety rules: its
own network `setu-install-net`, the assets served by `python3 -m
http.server` on the host's address on that network (port 8124, PID in
`$SETU_INSTALL_DIR/httpd.pid`), one container `setu-install-test` from
the e2e image (`--privileged --cgroupns=private`, `setu.test` →
127.0.0.1 in its `/etc/hosts`). It builds the release binary (musl, no
features), packages it with `scripts/package-release.sh`, runs
`install.sh --tls internal --panel-host setu.test --archive-url …
--sums-url …` in the box and asserts: install succeeds (`setu.toml`
setu 0600 with the unix listener, `/opt/setu/setu` root 0755);
`/healthz` 200 through Caddy and no TCP listener; `POST /setup` with a
wrong token 401, with the token file's token 201; login; the wizard's
`POST /setup/local-server` command (run with `SETU_JOIN_TOKEN`) joins
and the server is `online` within 60 s; `setu agent doctor` exit 0;
`setu backup` as `setu` writes a dump `pg_restore -l` lists; a second
`install.sh` keeps `setu.toml` and does not restart `setu.service`.
`SETU_INSTALL_KEEP=1` leaves the box up; diagnostics land in
`$SETU_INSTALL_DIR/diag-NN-<name>.txt`.

## Verifying a published release

`scripts/verify-release.sh <version>` is the public-release check: a
fresh systemd container (the same image and host rules as `e2e.sh`)
installs `v<version>` from github.com/setucp/setu, creates the owner,
joins the local agent, publishes the Miniflux and hello-axum manifests
from github.com/setucp/manifests and provisions both. Nine TAP
assertions end in `VERIFY PASS` or `VERIFY FAIL`. It needs outbound
HTTPS from the container; `SETU_VERIFY_KEEP=1` keeps the container for
inspection.
