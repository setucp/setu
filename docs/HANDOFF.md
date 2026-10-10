# Setu handoff kit

Everything a fresh Claude Code session needs to pick Setu up without
the conversations that built it.

## Where things stand (2026-10-10)

**Phase 1 (0.1.0) is built on branch `mvp`.** Next: the whole-branch
review, then the merge into `main` (PR against `origin/main`), then the
0.1.0 release when the owner decides (spec §12: name, org, licence,
public visibility). Phase 2 planning starts from
[ROADMAP.md](ROADMAP.md), with its own spec and plan written against
the real crate interfaces.

Gates recorded at the bottom of the ledger: `cargo fmt --check`,
`cargo clippy --workspace --all-targets --all-features -- -D warnings`,
`scripts/test.sh` (46 suites), the UI checks, `scripts/e2e.sh` (14
assertions, `E2E PASS`), `scripts/test-install.sh` (9 assertions,
`INSTALL PASS`), `scripts/check-links.sh`. CI on GitHub has not run
the e2e yet.

## Read in this order

- Spec: [specs/2026-10-09-setu-design.md](specs/2026-10-09-setu-design.md),
  **revision 4 (as built)**: revision 3 with every departure folded in
  and cited by ruling id. The spec is the authority.
- Review record: [specs/2026-10-09-setu-review.md](specs/2026-10-09-setu-review.md)
  (two review passes, then the revision 3 → as-built table and the
  outcome of the items the reviewers could not verify).
- Plan: [plans/2026-10-09-setu-mvp.md](plans/2026-10-09-setu-mvp.md)
  revision 3 (Tasks 0–15 with 10a–10d), the gap work list
  [plans/2026-10-09-rev3-gap.md](plans/2026-10-09-rev3-gap.md)
  (WS1–WS12) and the ledger
  [plans/2026-10-09-setu-mvp.ledger.md](plans/2026-10-09-setu-mvp.ledger.md)
  (rulings R1–R129, a "Superseded" list at the end, gate outputs).
- User docs: [README.md](README.md) indexes INSTALL, OPERATIONS,
  MANIFEST, AGENT, SECURITY, RELEASING and E2E.
- The repo root holds `CLAUDE.md` (working rules and the crate layout),
  `.claude/agents/` (rust-implementer, ui-implementer, code-reviewer,
  e2e-verifier) and `.claude/skills/` (setu-testing, setu-e2e,
  setu-conventions).

## Known items for the final review

Listed at the end of the ledger's WS sections ("Known, not fixed
here"): the runner logs an error when finishing a delete job whose row
the site's `ON DELETE CASCADE` removed; the heartbeat runs the helper's
`unit show` (and so `du -sb` as root) per site every 15 s without
caching; backup and restore buffer a whole archive in memory.

## Start prompt for a review session

> Review branch `mvp` of /home/gopal/Desktop/wp/setu against its spec (docs/specs/2026-10-09-setu-design.md, revision 4) and ledger (docs/plans/2026-10-09-setu-mvp.ledger.md). Use the repo's code-reviewer agent and superpowers:requesting-code-review, then superpowers:finishing-a-development-branch. Owner rules: no Claude co-author trailers; no remote push or public repo without me; never touch Postgres on 5432; one heavy cargo job at a time; never pkill by name.

## Owner decisions still open (spec §12)

Name, GitHub org, licence, whether the Cloudflare tunnel alternative to
Caddy lands in phase 1 (recommendation: phase 2). None blocks the
merge; the name only changes binary names and the UI title.
