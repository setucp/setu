#!/usr/bin/env bash
# Shell unit tests for install.sh (0.1.2 Task 3): architecture and
# distribution selection, the PGDG key pin and the forced plan. The
# distribution cases use the same os-release fixtures as the Rust table
# (crates/agent/tests/fixtures/os-release). Prints `ok`/`not ok` lines and
# exits non-zero on any failure. Nothing here runs as root or touches apt.
#
#   bash scripts/test-install-sh-unit.sh
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
FIX=$ROOT/crates/agent/tests/fixtures/os-release
SCRATCH=$(mktemp -d)
trap 'rm -rf "$SCRATCH"' EXIT

# Define install.sh's functions without running the installer.
export SETU_INSTALL_SOURCE_ONLY=1
# shellcheck source=../install.sh
. "$ROOT/install.sh"

t_select_archive_x86_64() {
    [[ "$(select_archive x86_64)" == x86_64-unknown-linux-musl ]]
}

t_select_archive_aarch64() {
    [[ "$(select_archive aarch64)" == aarch64-unknown-linux-musl ]] &&
        [[ "$(select_archive arm64)" == aarch64-unknown-linux-musl ]]
}

t_select_archive_other_fails() {
    ! select_archive riscv64 >/dev/null 2>&1 && ! select_archive i686 >/dev/null 2>&1
}

t_detect_each_fixture() {
    [[ "$(detect_distro "$FIX/ubuntu-26.04")" == "ubuntu 26.04 ubuntu-26.04" ]] &&
    [[ "$(detect_distro "$FIX/ubuntu-24.04")" == "ubuntu 24.04 ubuntu-24.04" ]] &&
        [[ "$(detect_distro "$FIX/ubuntu-22.04")" == "ubuntu 22.04 ubuntu-22.04" ]] &&
        [[ "$(detect_distro "$FIX/debian-12")" == "debian 12 debian-12" ]] &&
        [[ "$(detect_distro "$FIX/debian-13")" == "debian 13 debian-13" ]] &&
        [[ "$(detect_distro "$FIX/fedora-40")" == "fedora 40 unsupported" ]]
}

t_detect_missing_file_is_unsupported() {
    [[ "$(detect_distro "$SCRATCH/absent")" == "unknown unknown unsupported" ]]
}

t_force_distro_prints_plan() {
    PLAN=""
    resolve_plan fedora 40 unsupported 1 >"$SCRATCH/forced.out"
    [[ "$PLAN" == debian-12 ]] &&
        grep -q '^using the Debian 12 plan for fedora 40' "$SCRATCH/forced.out"
}

t_unsupported_refused_without_force() {
    local rc=0
    resolve_plan fedora 40 unsupported 0 >/dev/null 2>"$SCRATCH/refused.err" || rc=$?
    [[ "$rc" == 2 ]] && grep -q 'unsupported distribution fedora 40' "$SCRATCH/refused.err"
}

t_supported_plan_is_used_as_detected() {
    PLAN=""
    resolve_plan debian 13 debian-13 0
    [[ "$PLAN" == debian-13 ]]
}

t_pgdg_codenames() {
    [[ "$(pgdg_codename ubuntu-22.04)" == jammy ]] &&
        [[ "$(pgdg_codename debian-12)" == bookworm ]] &&
        ! pgdg_codename ubuntu-24.04 >/dev/null 2>&1 &&
        ! pgdg_codename debian-13 >/dev/null 2>&1
}

t_postgres_majors() {
    [[ "$(postgres_major ubuntu-24.04)" == 16 ]] &&
        [[ "$(postgres_major ubuntu-22.04)" == 16 ]] &&
        [[ "$(postgres_major debian-12)" == 16 ]] &&
        [[ "$(postgres_major debian-13)" == 17 ]] &&
        [[ "$(postgres_major ubuntu-26.04)" == 18 ]] &&
        ! pgdg_codename ubuntu-26.04 >/dev/null 2>&1
}

t_pgdg_list_line() {
    [[ "$(pgdg_list_line bookworm)" == "deb [signed-by=$PGDG_KEYRING] https://apt.postgresql.org/pub/repos/apt bookworm-pgdg main" ]]
}

# A wrong keyring must fail the check (install.sh stops before apt).
t_pgdg_key_mismatch_aborts() {
    printf 'not the PostgreSQL key\n' >"$SCRATCH/wrong.asc"
    ! verify_pgdg_key "$SCRATCH/wrong.asc" 2>/dev/null
}

# The shell pin is the Rust pin (crates/agent/src/distro.rs).
t_pgdg_pin_matches_rust_table() {
    grep -q "PGDG_KEY_SHA256: &str =" "$ROOT/crates/agent/src/distro.rs" &&
        grep -q "\"$PGDG_KEY_SHA256\"" "$ROOT/crates/agent/src/distro.rs"
}

# os-release values outside [A-Za-z0-9._-] are not distributions: they
# are reported as unknown (so the whitespace-split read in install.sh stays
# aligned) and refused as unsupported (0.1.2 M1).
t_detect_rejects_whitespace_id() {
    printf 'ID="ubuntu x"\nVERSION_ID="24.04"\n' >"$SCRATCH/space-id"
    printf 'ID=ubuntu\nVERSION_ID="24 .04"\n' >"$SCRATCH/space-version"
    [[ "$(detect_distro "$SCRATCH/space-id")" == "unknown unknown unsupported" ]] &&
        [[ "$(detect_distro "$SCRATCH/space-version")" == "unknown unknown unsupported" ]]
}

failed=0
count=0
for t in $(declare -F | awk '{print $3}' | grep '^t_' | sort); do
    count=$((count + 1))
    if (set +e; "$t"); then
        echo "ok $count - ${t#t_}"
    else
        echo "not ok $count - ${t#t_}"
        failed=$((failed + 1))
    fi
done
echo "1..$count ($failed failed)"
[[ "$failed" == 0 ]]
