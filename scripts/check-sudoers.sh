#!/usr/bin/env bash
# Golden check of the agent sudoers asset (spec §4) plus `visudo -cf`.
#   bash /home/gopal/Desktop/wp/setu-work-1/scripts/check-sudoers.sh
# The asset is embedded in the agent (`install::SUDOERS_ASSET`) and
# compared byte for byte in crates/agent/tests/golden.rs as well.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
ASSET="${REPO_ROOT}/crates/agent/assets/sudoers"

fail() {
    echo "check-sudoers: FAIL: $1" >&2
    exit 1
}

[[ -f "${ASSET}" ]] || fail "missing ${ASSET}"

EXPECTED='setu ALL=(root) NOPASSWD: /opt/setu/setu helper *
setu ALL=(postgres) NOPASSWD: /usr/bin/psql, /usr/bin/pg_dump, /usr/bin/pg_restore'
[[ "$(cat "${ASSET}")" == "${EXPECTED}" ]] || fail "asset differs from the spec §4 golden text"

# Prefer classic sudo's visudo (Ubuntu 24.04 ships it); Ubuntu >= 25.10
# defaults to sudo-rs and keeps the classic one as visudo.ws. The asset
# has no `Defaults` line (ledger R70), so sudo-rs's visudo parses it too.
VISUDO=""
for cand in visudo.ws visudo; do
    if command -v "${cand}" >/dev/null 2>&1; then
        VISUDO="$(command -v "${cand}")"
        break
    fi
done
if [[ -z "${VISUDO}" ]]; then
    echo "check-sudoers: ok (golden text); SKIPPED visudo -cf: visudo is not installed"
    exit 0
fi
# visudo may insist on root ownership and 0440: check a copy with those
# attributes when we can (root), else the 0440 copy as-is.
TMP="$(mktemp -d)"
trap 'rm -rf "${TMP}"' EXIT
install -m 0440 "${ASSET}" "${TMP}/setu"
if [[ "$(id -u)" == "0" ]]; then
    chown root:root "${TMP}/setu"
fi
"${VISUDO}" -cf "${TMP}/setu" >/dev/null || fail "visudo -cf rejected the asset"
echo "check-sudoers: ok (golden text + ${VISUDO} -cf)"
