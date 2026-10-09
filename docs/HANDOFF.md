# Setu handoff kit

Everything a fresh Claude Code session needs to build the Setu MVP
without this conversation.

- Spec: `specs/2026-10-09-setu-design.md`, revision 3 after two
  independent review passes; the findings and resolutions are in
  `specs/2026-10-09-setu-review.md`. Open questions in §12 are the owner's.
- Roadmap for phases 2–4: `ROADMAP.md` (scope only; plans are written per phase).
- Plan: `plans/2026-10-09-setu-mvp.md` revision 3 (Tasks 0–15 with
  10a–10d, TDD, subagent-driven). Ledger goes to
  `plans/2026-10-09-setu-mvp.ledger.md`.
- The repo root holds `CLAUDE.md`, `.claude/agents/` (rust-implementer,
  ui-implementer, code-reviewer, e2e-verifier), `.claude/skills/`
  (setu-testing, setu-e2e, setu-conventions) and the toolchain, clippy
  and cargo-deny configs. Task 0 edits the toolchain and deny files.

## Start prompt for the implementing session

Open Claude Code in `/home/gopal/Desktop/wp` and paste:

> Implement the Setu MVP. Spec: /home/gopal/Desktop/wp/setu/docs/specs/2026-10-09-setu-design.md. Plan: /home/gopal/Desktop/wp/setu/docs/plans/2026-10-09-setu-mvp.md. Handoff kit: /home/gopal/Desktop/wp/setu/docs/HANDOFF.md. Use superpowers:subagent-driven-development with the repo agents from the skeleton (rust-implementer, ui-implementer, code-reviewer, e2e-verifier). Start at Task 0. Record rulings in the ledger next to the plan. Owner rules: no Claude co-author trailers; no remote push or public repo without me; never touch Postgres on 5432; one heavy cargo job at a time; never pkill by name.

Execution method: subagent-driven (per-task reviewer) — the agent and
jobs crates carry security-sensitive code (sudo surface, privileged file
writes, secrets) where a second pair of eyes per task is worth the cost.
Fallback: executing-plans inline if the session budget is tight.

## Owner decisions still open (spec §12)

Name, GitHub org, licence, whether the Cloudflare tunnel alternative to
Caddy lands in phase 1. None blocks Tasks 0–15; the name only changes
binary names and the UI title (one `sed` later).

## Order of magnitude

Nineteen task units; roughly 12–16k lines of Rust and 4–6k of TypeScript.
Expect several sessions. The e2e (Task 11) is the gate that proves the
product; nothing after it is worth doing if it is red.
