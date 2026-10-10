#!/usr/bin/env bash
# Fails when the tracked tree contains strings that belong to a private
# host or person (local paths, personal domains, session scratch paths).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
pattern='gopal|cppprograms|/home/[a-z]+/|claude-1000|scratchpad|172\.30\.0\.|vyasa-internal|setu-internal/|setu-work-'
if hits=$(git -C "$ROOT" grep -nIE "$pattern" -- . ':!scripts/check-public-tree.sh' 2>/dev/null) && [ -n "$hits" ]; then
  echo "private references in the public tree:" >&2; echo "$hits" >&2; exit 1
fi
echo "check-public-tree: clean"
