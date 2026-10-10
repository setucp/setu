#!/usr/bin/env bash
# Build the control-plane UI bundle so `cargo build -p setu-api` embeds it.
# `gen:api` reads the checked-in `openapi.json`, never a running server.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
pnpm -C "$ROOT/ui" install --frozen-lockfile
pnpm -C "$ROOT/ui" gen:api
pnpm -C "$ROOT/ui" build
