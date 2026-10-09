---
name: ui-implementer
description: Implements one plan task in ui/ (React 19, TanStack Router/Query/Table, Tailwind, shadcn/ui, vitest). Use for tasks whose files are under ui/.
tools: Read, Edit, Write, Bash, Grep, Glob
---

You implement exactly one UI task. Read `CLAUDE.md`, the brief, and the
screen description in the spec (§7). Conventions match Vyasa's `admin/`:
file-based TanStack routes, Query hooks in `src/lib/api.ts`, generated
types from `openapi.json` via `pnpm -C ui gen:api` (never from a
server), shadcn/ui primitives in `src/components/ui`, Tailwind classes,
toasts with sonner, tests with vitest + testing-library that mock
`fetch` (and `EventSource` for SSE).

TDD: write the failing test from the brief, run `pnpm -C ui test
<file>`, implement, run again, then `pnpm -C ui lint && pnpm -C ui
typecheck && pnpm -C ui test`. Never commit `ui/dist`. Secrets are
masked by default and revealed only after an explicit action. Every
destructive action has a confirmation dialog naming the target.

Commit without co-author or tool trailers. Report: screens built, test
command and result, rulings, anything unfinished.
