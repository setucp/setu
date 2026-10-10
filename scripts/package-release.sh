#!/usr/bin/env bash
# Packages one architecture's release assets (spec §8, 0.1.2 §1):
#   scripts/package-release.sh VERSION BINDIR DISTDIR [TARGET]
# TARGET (or the TARGET environment variable) is x86_64-unknown-linux-musl
# (default) or aarch64-unknown-linux-musl.
#
#   <dist>/setu-<ver>-<target>.tar.gz     one top-level `setu`
#   <dist>/install.sh                     the installer (architecture-neutral)
#   <dist>/SHA256SUMS                     one line per archive of <ver> in <dist>, and install.sh
#
# Run once per architecture into the same DISTDIR: SHA256SUMS is rebuilt
# from every archive of that version present, so the last run lists all.
# The binary must be built without the `test-root` and `e2e-hooks`
# features; the release workflow and scripts/test-install.sh build it so.
set -euo pipefail

[[ $# == 3 || $# == 4 ]] || { echo "usage: package-release.sh VERSION BINDIR DISTDIR [TARGET]" >&2; exit 1; }
version="$1"; bindir="$2"; distdir="$3"
target="${4:-${TARGET:-x86_64-unknown-linux-musl}}"
case "$target" in
    x86_64-unknown-linux-musl | aarch64-unknown-linux-musl) ;;
    *) echo "package-release.sh: unsupported target $target" >&2; exit 1 ;;
esac
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
# Only the exact release names of the two targets; other files in DISTDIR
# (stray archives, other versions) are not release assets.
archives=()
for t in x86_64-unknown-linux-musl aarch64-unknown-linux-musl; do
  [[ -f "${distdir}/setu-${version}-${t}.tar.gz" ]] && archives+=("setu-${version}-${t}.tar.gz")
done
(cd "$distdir" && sha256sum "${archives[@]}" install.sh > SHA256SUMS)
echo "packaged ${distdir}/${name}.tar.gz; SHA256SUMS lists ${#archives[@]} archive(s) and install.sh"
