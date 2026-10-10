#!/usr/bin/env bash
# Throwaway Postgres for Setu tests.
#   bash /home/gopal/Desktop/wp/setu-work-1/scripts/test-pg.sh up|down|status
# Container `setu-test-pg`, port ${SETU_TEST_PG_PORT:-55440}. Never 5432.
set -euo pipefail

CONTAINER_NAME="setu-test-pg"
PORT="${SETU_TEST_PG_PORT:-55440}"
IMAGE="postgres:16-alpine"

refuse_5432() {
    if [[ "${PORT}" == "5432" ]]; then
        echo "test-pg.sh: refusing to use port 5432 (production Postgres)" >&2
        exit 1
    fi
}

container_running() {
    [[ "$(docker inspect -f '{{.State.Running}}' "${CONTAINER_NAME}" 2>/dev/null || echo missing)" == "true" ]]
}

container_host_port() {
    docker inspect -f '{{(index (index .NetworkSettings.Ports "5432/tcp") 0).HostPort}}' "${CONTAINER_NAME}" 2>/dev/null || echo ""
}

start_container() {
    docker rm -f "${CONTAINER_NAME}" >/dev/null 2>&1 || true
    docker run -d --name "${CONTAINER_NAME}" \
        -e POSTGRES_USER=setu \
        -e POSTGRES_PASSWORD=setu \
        -p "127.0.0.1:${PORT}:5432" \
        "${IMAGE}" >/dev/null
    echo "test-pg: started ${CONTAINER_NAME} on 127.0.0.1:${PORT}"
}

cmd_up() {
    refuse_5432
    if container_running; then
        ACTUAL_PORT="$(container_host_port)"
        if [[ "${ACTUAL_PORT}" == "${PORT}" ]]; then
            echo "test-pg: container ${CONTAINER_NAME} already running (port ${PORT})"
        else
            echo "test-pg: container ${CONTAINER_NAME} is on port ${ACTUAL_PORT:-unknown}, want ${PORT}; recreating" >&2
            start_container
        fi
    else
        start_container
    fi
    for _ in $(seq 1 30); do
        if docker exec "${CONTAINER_NAME}" pg_isready -U setu -d postgres >/dev/null 2>&1; then
            echo "test-pg: ready on 127.0.0.1:${PORT}"
            return 0
        fi
        sleep 1
    done
    echo "test-pg: timed out waiting for Postgres in ${CONTAINER_NAME}" >&2
    docker logs "${CONTAINER_NAME}" 2>&1 | tail -n 20 >&2 || true
    exit 1
}

cmd_down() {
    docker rm -f "${CONTAINER_NAME}" >/dev/null 2>&1 || true
    echo "test-pg: removed ${CONTAINER_NAME} (if it existed)"
}

cmd_status() {
    refuse_5432
    if ! docker inspect "${CONTAINER_NAME}" >/dev/null 2>&1; then
        echo "test-pg: container ${CONTAINER_NAME} absent"
        exit 1
    fi
    docker inspect -f 'test-pg: {{.Name}} running={{.State.Running}} ports={{json .NetworkSettings.Ports}}' "${CONTAINER_NAME}"
    if docker exec "${CONTAINER_NAME}" pg_isready -U setu -d postgres 2>&1; then
        return 0
    fi
    echo "test-pg: container present but Postgres not ready" >&2
    exit 1
}

case "${1:-}" in
    up) cmd_up ;;
    down) cmd_down ;;
    status) cmd_status ;;
    *) echo "usage: test-pg.sh up|down|status" >&2; exit 2 ;;
esac
