---
name: setu-conventions
description: Setu code conventions — error envelope, ids, sealing, idempotent agent commands, authz roles, audit, UI patterns. Use when writing any new handler, command, repo method or screen.
---

# Conventions

- IDs: ULID strings; `short_id` = 8 chars, unique per server, used in
  every name: user `setu-site-<short>`, unit `setu-site@<short>.service`,
  db and role `site_<short>`, dir `/srv/setu/sites/<short>`.
- Errors: `AppError { code, message, status }` → `{ "error": { code, message } }`.
  Codes: validation 400, unauthorized 401, forbidden 403, not_found 404,
  conflict 409, agent_unreachable 502, internal 500. Never leak internal
  error text for `internal`; log it with `tracing::error!` instead.
- Secrets: sealed with `Sealer` (`v1:`/`plain:` prefixes, KeyVault
  scheme) before any DB write; `agent_commands` secret fields nulled on
  completion; agent redacts logs; API returns masked values; reveal
  only via `POST /sites/{id}/env/{name}/reveal` with password re-auth;
  manual `Debug` redacts.
- Agent commands: idempotent (check state, then act), deduped by id,
  serialised per site, rejected when `expires_at` passed. Result
  `ok=false` carries a one-line redacted `error` and the log.
- Privilege: the agent never writes outside `/var/lib/setu`; every
  privileged action is a typed `setu helper <verb> --site <short>` with
  JSON on stdin; paths derive from the short id only; no free paths,
  modes or owners ever cross the sudo boundary. Postgres passwords go
  to psql as `-v` variables on stdin, never argv. Caddy only via the
  unix socket.
- Jobs: named steps with a stored cursor, each logs `step <name>:
  ok|error …`; leases (`locked_until`) renewed while running; a failure
  sets the site `failed` with `failed_step` (rollback leaves it
  `running` on the old version); never leave `provisioning` or
  `updating` on exit.
- Authz: `owner` > `operator` > `viewer`; mutating routes audit
  `(operator_id, action, target)`.
- UI: list → detail; status badges (`running` green, `failed` red,
  `provisioning|updating` amber, `stopped` gray); destructive actions
  confirm with the target's name; secrets masked, reveal is explicit;
  long operations navigate to the job page with a live log.
- Docs: a user-visible change updates `docs/` (and `CHANGELOG.md` once
  Task 14 creates it) in the same commit.
