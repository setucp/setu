# Security

## Operators

Passwords are argon2id (default params); unknown-email logins verify
against a baked-in dummy hash so their timing matches a wrong-password
attempt, and both return the identical 401. Ten failures per IP in
15 minutes → 429. Sessions live in Postgres (tower-sessions,
24-hour inactivity expiry); the session id rotates at login. CSRF:
same-site `Lax` cookies plus an Origin-vs-Host check on every
mutating request (no `Origin` header, e.g. curl, passes). Roles:
`owner` > `operator` > `viewer` (`viewer` reads, `operator` manages
everything except operators and plans, `owner` everything); every
route × role is pinned by `crates/api/tests/matrix.rs`.

## Tokens and secrets

Server tokens are 32 random bytes with a kind prefix (`setu_join_…`,
`setu_srv_…`); only sha256 hashes are stored. Join tokens are
single-use with 1-hour expiry (the redeem re-checks under the row
lock, so racing joins cannot both succeed). Site secrets are sealed
at rest under `SETU_SECRET_KEY` (required, 32 bytes as 64 hex chars;
boot refuses otherwise) with ChaCha20-Poly1305 `v1:` envelopes; plain
values never carry the prefix, logs never print env values, and every
struct holding secrets has a redacting `Debug`. `GET /sites/{id}/env`
masks secret values.

## Isolation

Each site runs as its own user with `ProtectSystem=strict`,
`NoNewPrivileges=yes`, `PrivateTmp=yes`, memory and CPU limits from
the plan; the agent never runs app code as `setu`. The agent's only
root path is the pinned helper (`setu helper …` + trailing ` *`,
since sudo matches argv-exactly and per-site units vary per call):
verbs, units, paths and usernames are allowlisted inside the helper
itself (unit-tested); Postgres admin is confined to
`psql`/`pg_dump`/`pg_restore` as the `postgres` user. No shells, no
editors, no package managers. `scripts/check-sudoers.sh` asserts the
asset matches the golden file and `visudo -cf` accepts it.

## Audit and limits

Every mutating operator action writes `audit_log` (operator, action,
target, IP). Agent command polling is rate-limited per server. No
telemetry.
