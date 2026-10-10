#!/usr/bin/env bash
# Run the Rust workspace tests against the throwaway Postgres container.
#   CARGO_BUILD_JOBS=4 bash /home/gopal/Desktop/wp/setu-work-1/scripts/test.sh [-p crate] [test-name...]
# Ensures test-pg is up, exports SETU_TEST_PG_URL, keeps the container after the run.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
CONTAINER_NAME="setu-test-pg"
PORT="${SETU_TEST_PG_PORT:-55440}"

if [[ "${PORT}" == "5432" ]]; then
    echo "test.sh: refusing to use port 5432 (production Postgres)" >&2
    exit 1
fi

pg_reachable() {
    if [[ "$(docker inspect -f '{{.State.Running}}' "${CONTAINER_NAME}" 2>/dev/null || echo missing)" == "true" ]]; then
        # Our container exists: it counts only if Postgres itself answers.
        if docker exec "${CONTAINER_NAME}" pg_isready -U setu -d postgres >/dev/null 2>&1; then
            return 0
        fi
        return 1
    fi
    # No container: CI services (or a manually provisioned Postgres) may
    # serve the port. Prefer pg_isready so a stray TCP listener is not
    # mistaken for Postgres; fall back to attempting test-pg.sh up when
    # unsure (no pg_isready tool available).
    if command -v pg_isready >/dev/null 2>&1; then
        if pg_isready -h 127.0.0.1 -p "${PORT}" >/dev/null 2>&1; then
            return 0
        fi
        return 1
    fi
    # Last resort without pg_isready (keeps CI compat where a real PG is
    # already serving): treat any TCP listener as usable.
    if timeout 2 bash -c "</dev/tcp/127.0.0.1/${PORT}" 2>/dev/null; then
        return 0
    fi
    return 1
}

if ! pg_reachable; then
    bash "${SCRIPT_DIR}/test-pg.sh" up
fi

export SETU_TEST_PG_PORT="${PORT}"
export SETU_TEST_PG_URL="postgres://setu:setu@127.0.0.1:${PORT}/postgres"

# "$@" must precede `--` so filters like `-p setu-testkit` reach cargo, not the test binary.
exec cargo test --workspace "$@" -- --test-threads=4
