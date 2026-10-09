#!/usr/bin/env bash
# Packages one release archive: scripts/package-release.sh VERSION TARGET BINDIR DISTDIR
# Layout: <dist>/setu-<ver>-<target>.tar.gz holding ONE top-level `setu`
# binary (what install.sh asserts) plus <name>.tar.gz.sha256.
set -euo pipefail

[[ $# == 4 ]] || { echo "usage: package-release.sh VERSION TARGET BINDIR DISTDIR" >&2; exit 1; }
version="$1"; target="$2"; bindir="$3"; distdir="$4"
name="setu-${version}-${target}"

[[ -x "${bindir}/setu" ]] || { echo "package-release.sh: no setu binary in $bindir" >&2; exit 1; }
mkdir -p "$distdir"
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
cp "${bindir}/setu" "$tmp/setu"
tar -czf "${distdir}/${name}.tar.gz" -C "$tmp" setu
(cd "$distdir" && sha256sum "${name}.tar.gz" > "${name}.tar.gz.sha256")
echo "packaged ${distdir}/${name}.tar.gz"
