#!/usr/bin/env bash
# Packages the release assets (spec §8): scripts/package-release.sh VERSION BINDIR DISTDIR
#
#   <dist>/setu-<ver>-x86_64-unknown-linux-musl.tar.gz   one top-level `setu`
#   <dist>/install.sh                                    the installer
#   <dist>/SHA256SUMS                                    `sha256sum` lines for both
#
# x86_64 musl is the only release target (aarch64 is phase 2). The
# binary must be built without the `test-root` and `e2e-hooks`
# features; the release workflow and scripts/test-install.sh build it so.
set -euo pipefail

[[ $# == 3 ]] || { echo "usage: package-release.sh VERSION BINDIR DISTDIR" >&2; exit 1; }
version="$1"; bindir="$2"; distdir="$3"
target="x86_64-unknown-linux-musl"
name="setu-${version}-${target}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]] ||
    { echo "package-release.sh: bad version $version" >&2; exit 1; }
[[ -x "${bindir}/setu" ]] || { echo "package-release.sh: no setu binary in $bindir" >&2; exit 1; }
mkdir -p "$distdir"
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
install -m 0755 "${bindir}/setu" "$tmp/setu"
tar -C "$tmp" --owner=0 --group=0 --numeric-owner -czf "${distdir}/${name}.tar.gz" setu
install -m 0755 "${root}/install.sh" "${distdir}/install.sh"
(cd "$distdir" && sha256sum "${name}.tar.gz" install.sh > SHA256SUMS)
echo "packaged ${distdir}/${name}.tar.gz, install.sh and SHA256SUMS"
