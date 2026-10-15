#!/usr/bin/env bash
# Packaging test (0.1.2 Task 5): two architectures packaged into one dist
# directory give two archives with distinct names and one SHA256SUMS with
# exactly three lines (both archives and install.sh), each verifying.
#
#   bash scripts/test-package-release.sh
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
SCRATCH=$(mktemp -d)
trap 'rm -rf "$SCRATCH"' EXIT
VERSION=0.1.2
X86=x86_64-unknown-linux-musl
ARM=aarch64-unknown-linux-musl
DIST=$SCRATCH/dist

mkdir -p "$SCRATCH/bin-x86" "$SCRATCH/bin-arm"
printf 'x86 build\n' >"$SCRATCH/bin-x86/setu"
printf 'arm build\n' >"$SCRATCH/bin-arm/setu"
chmod 0755 "$SCRATCH/bin-x86/setu" "$SCRATCH/bin-arm/setu"

TARGET=$X86 bash "$ROOT/scripts/package-release.sh" "$VERSION" "$SCRATCH/bin-x86" "$DIST" >/dev/null
bash "$ROOT/scripts/package-release.sh" "$VERSION" "$SCRATCH/bin-arm" "$DIST" "$ARM" >/dev/null

fail() { echo "not ok - $1"; exit 1; }

[ -f "$DIST/setu-$VERSION-$X86.tar.gz" ] || fail "x86_64 archive missing"
[ -f "$DIST/setu-$VERSION-$ARM.tar.gz" ] || fail "aarch64 archive missing"
echo "ok - both archives present"

lines=$(wc -l <"$DIST/SHA256SUMS")
[ "$lines" = 3 ] || fail "SHA256SUMS has $lines lines, want 3"
names=$(awk '{print $2}' "$DIST/SHA256SUMS" | sort -u | wc -l)
[ "$names" = 3 ] || fail "SHA256SUMS names are not distinct"
grep -q "setu-$VERSION-$X86.tar.gz" "$DIST/SHA256SUMS" || fail "x86_64 archive not listed"
grep -q "setu-$VERSION-$ARM.tar.gz" "$DIST/SHA256SUMS" || fail "aarch64 archive not listed"
grep -q ' install.sh$' "$DIST/SHA256SUMS" || fail "install.sh not listed"
echo "ok - SHA256SUMS lists exactly the two archives and install.sh"

(cd "$DIST" && sha256sum -c --quiet SHA256SUMS) || fail "SHA256SUMS does not verify"
echo "ok - SHA256SUMS verifies"

# The aarch64 archive carries the aarch64 build, not the x86_64 one.
tar -xzf "$DIST/setu-$VERSION-$ARM.tar.gz" -C "$SCRATCH" setu
grep -q 'arm build' "$SCRATCH/setu" || fail "aarch64 archive does not hold the aarch64 build"
echo "ok - each archive holds its own build"

if TARGET=riscv64-unknown-linux-musl bash "$ROOT/scripts/package-release.sh" "$VERSION" "$SCRATCH/bin-x86" "$SCRATCH/other" >/dev/null 2>&1; then
  fail "an unknown target must be refused"
fi
echo "ok - unknown target refused"

# A stray archive in DISTDIR is not a release asset: only the exact
# setu-<ver>-<target>.tar.gz names of the two targets are listed.
printf 'stray\n' >"$DIST/setu-$VERSION-riscv64-unknown-linux-musl.tar.gz"
bash "$ROOT/scripts/package-release.sh" "$VERSION" "$SCRATCH/bin-x86" "$DIST" "$X86" >/dev/null
lines=$(wc -l <"$DIST/SHA256SUMS")
[ "$lines" = 3 ] || fail "a stray archive is listed: SHA256SUMS has $lines lines, want 3"
grep -q riscv64 "$DIST/SHA256SUMS" && fail "a stray archive is listed in SHA256SUMS"
echo "ok - stray archives are not listed"
echo "1..6 PASS"
