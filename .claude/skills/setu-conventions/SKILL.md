---
name: setu-conventions
description: Setu code conventions — error envelope, ids, sealing, idempotent agent commands, authz roles, audit, UI patterns. Use when writing any new handler, command, repo method or screen.
---

# Conventions

- IDs: ULID strings; `short_id` = 8 chars, unique per server, used in
  every name: user `setu-site-<short>`, unit `setu-site@<short>.service`,
  db and role `site_<short>`, dir `/srv/setu/sites/<short>`.
- Errors: `AppError { code, message }` → `{ "error": { code, message } }` (status derived from the code).
  Codes: validation 400, unauthorized 401, forbidden 403, not_found 404,
  conflict 409, rate_limited 429, agent_unreachable 502, internal 500. Never leak internal
  error text for `internal`; log it with `tracing::error!` instead.
- Secrets: sealed with `Sealer` (`v1:`/`plain:` prefixes, KeyVault
  scheme) before any DB write; `agent_commands` secret fields nulled on
  completion, cancel, expiry and prune (`setu_redact_command`); the
  agent replaces every password and env value a command carried with
  `***` in its result; job log lines mask registered secrets; API
  returns masked values (`postgres_url`-sourced ones always); reveal
  only via `POST /sites/{id}/env/{name}/reveal` with password re-auth
  (operator+, wrong password 403, audited `env.reveal` /
  `env.reveal_denied`); manual `Debug` redacts. Job payload secrets are
  `v1:` sealed whatever the `secret` flag.
- Agent commands: idempotent (check state, then act), deduped by id,
  serialised per site (read-only and database commands take no slot),
  rejected when `expires_at` passed, result persisted to `state.json`
  before it is posted. `site` is the short id except in `SiteCreate`,
  `WriteUnit` and `Health` (which carry `SiteSpec`); route commands
  carry `site_short_id`. Result `ok=false` carries a one-line redacted
  `error` and the log.
- Privilege: the agent never writes outside `/var/lib/setu`; every
  privileged action is a typed `setu helper <verb> --site <short>` with
  JSON on stdin; paths derive from the short id only; no free paths,
  modes or owners ever cross the sudo boundary. Postgres passwords go
  to psql as a `\set pw '…'` line on stdin, never argv; `pg_dump` gets
  `PGPASSWORD` in its environment. The helper re-validates version,
  exec, args and dir names; archives stream over stdin/stdout; `--root`
  only under the `test-root` feature and never under sudo. Helper
  `unit start` = `systemctl enable --now`, `stop` = `disable --now`.
  `sudo -n` always. The sudoers file is exactly two lines (helper,
  postgres). Caddy only via the unix socket (`GET /id` → `PATCH`, else
  `POST …/routes`; ids `setu-site-<short>`, `setu-panel`).
- Jobs: named steps with a stored cursor, each logs `step <name>:
  ok|error …`; leases (`locked_until`) renewed while running; a failure
  sets the site `failed` with `failed_step` (rollback leaves it
  `running` on the old version); never leave `provisioning` or
  `updating` on exit.
- Authz: `owner` > `operator` > `viewer`; mutating routes audit
  `(operator_id, action, target_kind, target_id, ip)`. Every mutation
  under `/api/v1` (login and setup too) needs `Origin` = `public_url`
  or `Sec-Fetch-Site: same-origin`; scripts send `Origin`.
- UI: list → detail; status badges (`running` green, `failed` red,
  `provisioning|updating` amber, `stopped` gray); destructive actions
  confirm with the target's name; secrets masked, reveal is explicit;
  long operations navigate to the job page with a live log.
- Docs: a user-visible change updates `docs/` and `CHANGELOG.md` in the
  same commit; a departure from the spec is a ledger ruling cited in
  the spec text (revision 4 does this, e.g. "(R118)").
