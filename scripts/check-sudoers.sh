#!/usr/bin/env bash
# Assert the agent sudoers asset matches the golden rules and parses.
#   bash /home/gopal/Desktop/wp/setu-work-1/scripts/check-sudoers.sh
# The asset is embedded in the agent (`install::SUDOERS_ASSET`), so
# `install` and this script can never disagree about its contents.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
ASSET="${REPO_ROOT}/crates/agent/assets/sudoers"

fail() {
    echo "check-sudoers: FAIL: $1" >&2
    exit 1
}

[[ -f "${ASSET}" ]] || fail "missing ${ASSET}"

# Required sudo surface (spec §4/§7): root only through the pinned
# helper. A bare `… helper` entry would match argv-exactly (sudo compares
# the full command line), so the rule carries a trailing ` *` and the
# helper allowlists verbs/units/paths itself (unit-tested in helper.rs).
# Postgres admin confined to the postgres user and the three binaries
# the agent needs.
required=(
    "setu ALL=(root) NOPASSWD: /opt/setu/setu helper *"
    "setu ALL=(postgres) NOPASSWD: /usr/bin/psql"
    "setu ALL=(postgres) NOPASSWD: /usr/bin/pg_dump"
    "setu ALL=(postgres) NOPASSWD: /usr/bin/pg_restore"
)
for needle in "${required[@]}"; do
    grep -qF -- "${needle}" "${ASSET}" || fail "asset missing: ${needle}"
done

# Exactly one wildcard form is allowed: the helper rule's trailing ` *`
# (its subcommands vary per call and are validated inside the helper).
# Anywhere else a wildcard signals an overbroad rule. No shells,
# editors, package managers, or direct systemctl/journalctl entries
# (those go through the helper, which validates verb and unit). Comments
# are stripped before this check so prose cannot trip it.
CODE_LINES="$(grep -v '^[[:space:]]*#' "${ASSET}" || true)"
# Scope out exactly the documented exception (the helper rule's trailing
# ` *`) before banning wildcards, so a second wildcard on that same line
# is still caught.
SCOPED="$(printf '%s\n' "${CODE_LINES}" | sed 's|/opt/setu/setu helper \*|HELPER_RULE|' || true)"
for bad in "*" "/bin/sh" "/bin/bash" "/usr/bin/vim" "ALL=(ALL) NOPASSWD: ALL" "/usr/bin/apt" "systemctl" "journalctl"; do
    printf '%s\n' "${SCOPED}" | grep -qF -- "${bad}" && fail "asset must not contain: ${bad}"
done

# Every non-comment line must be a rule for the agent user.
awk 'NF && $1 !~ /^#/ && $1 != "setu" { print "bad user: " $0; exit 1 }' "${ASSET}" \
    || fail "all rules must apply to user setu"

test -s "${ASSET}" || fail "asset is empty"

if command -v visudo >/dev/null 2>&1; then
    visudo -cf "${ASSET}" || fail "visudo -cf rejected the asset"
    echo "check-sudoers: ok (patterns + visudo -cf)"
else
    echo "check-sudoers: ok (patterns; visudo not available, skipped parse check)"
fi
