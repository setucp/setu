#!/usr/bin/env bash
# Prints the CHANGELOG.md section for VERSION (the release notes).
# Matches `## VERSION` with any suffix (`## 0.1.0 — Unreleased`,
# `## 0.1.0-rc.1 (2026-…)`); the next `## ` line ends the section.
# Usage: scripts/release-notes.sh 0.1.0
set -euo pipefail
[[ $# == 1 ]] || { echo "usage: release-notes.sh VERSION" >&2; exit 1; }
version="$1"
awk -v want="$version" '
  $1 == "##" && $2 == want { found = 1; next }
  found && $1 == "##" { exit }
  found { print }
' CHANGELOG.md
awk -v want="$version" '$1 == "##" && $2 == want { found = 1 } END { exit !found }' CHANGELOG.md \
    || { echo "release-notes.sh: no ## ${version} section in CHANGELOG.md" >&2; exit 1; }
