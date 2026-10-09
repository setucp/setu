#!/usr/bin/env bash
# Regenerate sqlx offline query data (.sqlx/) at the workspace root.
#
#   CARGO_BUILD_JOBS=4 bash /home/gopal/Desktop/wp/setu-work-1/scripts/sqlx-prepare.sh
#
# Uses the throwaway test Postgres (never 5432): creates a scratch
# database, runs crates/db/migrations on it, then `cargo sqlx prepare`
# with DATABASE_URL pointed at the migrated scratch DB so the
# query_as!/query! macros can type-check every query. The result is
# committed as .sqlx/ so later builds and tests compile without a live
# database (scripts/test.sh does not set DATABASE_URL).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
CONTAINER_NAME="setu-test-pg"
PORT="${SETU_TEST_PG_PORT:-55440}"

if [[ "${PORT}" == "5432" ]]; then
    echo "sqlx-prepare.sh: refusing to use port 5432 (production Postgres)" >&2
    exit 1
fi

if ! cargo sqlx --version >/dev/null 2>&1; then
    echo "sqlx-prepare.sh: cargo-sqlx not found; install it first:" >&2
    echo "  cargo install sqlx-cli --no-default-features --features postgres,rustls" >&2
    exit 1
fi

bash "${SCRIPT_DIR}/test-pg.sh" up >/dev/null

SCRATCH_DB="setu_prepare_$$"
export DATABASE_URL="postgres://setu:setu@127.0.0.1:${PORT}/${SCRATCH_DB}"

cleanup() {
    cargo sqlx database drop -y --database-url "${DATABASE_URL}" >/dev/null 2>&1 || true
}
trap cleanup EXIT

cargo sqlx database create --database-url "${DATABASE_URL}" >/dev/null
cargo sqlx migrate run \
    --source "${REPO_ROOT}/crates/db/migrations" \
    --database-url "${DATABASE_URL}" >/dev/null
echo "sqlx-prepare: scratch database migrated at 127.0.0.1:${PORT}/${SCRATCH_DB}"

cd "${REPO_ROOT}"
CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}" cargo sqlx prepare --workspace -- --all-targets
echo "sqlx-prepare: .sqlx/ regenerated"
