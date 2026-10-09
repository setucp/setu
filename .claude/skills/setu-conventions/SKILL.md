---
name: setu-conventions
description: Setu code conventions — error envelope, ids, sealing, idempotent agent commands, authz roles, audit, UI patterns. Use when writing any new handler, command, repo method or screen.
---

# Conventions

- IDs: ULID strings; `short_id` = last 8 chars; db `site_<short>`, user
  `setu-site-<id>`, unit `setu-site@<id>.service`.
- Errors: `AppError { code, message, status }` → `{ "error": { code, message } }`.
  Codes: validation 400, unauthorized 401, forbidden 403, not_found 404,
  conflict 409, agent_unreachable 502, internal 500. Never leak internal
  error text for `internal`; log it with `tracing::error!` instead.
- Secrets: sealed with `Sealer` (`v1:` prefix) before any DB write; API
  returns masked values plus `has_value`; manual `Debug` redacts.
- Agent commands: every handler must be safe to run twice (check state,
  then act). Result `ok=false` carries a one-line `error` and the log.
- Jobs: named steps, each logs `step <name>: ok|error …`; a failure
  sets the site `failed` with `failed_step`; never leave `provisioning`
  or `updating` on exit.
- Authz: `owner` > `operator` > `viewer`; mutating routes audit
  `(operator_id, action, target)`.
- UI: list → detail; status badges (`running` green, `failed` red,
  `provisioning|updating` amber, `stopped` gray); destructive actions
  confirm with the target's name; secrets masked, reveal is explicit;
  long operations navigate to the job page with a live log.
- Docs: a user-visible change updates `docs/` and `CHANGELOG.md` in the
  same commit.
