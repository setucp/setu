#!/usr/bin/env bash
# Offline relative-link check for tracked Markdown files: every
# `[text](path)` / `[text](path#anchor)` link whose target is a relative
# file path must resolve to a file in the repo. External (http...),
# absolute (/...), anchor-only (#...) and mailto: links are skipped.
# (Vyasa's check-links.sh shells to lychee via Docker; this repo has no
# such dependency, so the check is grep-based and runs anywhere.)
set -euo pipefail
REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

failures=0
while IFS= read -r -d '' file; do
    dir="$(dirname "$file")"
    # Extract markdown link targets: [text](target) and <target>? No —
    # reference-style and bare URLs are out of scope; inline only.
    while IFS= read -r target; do
        case "$target" in
            ""|"#"*|"http://"*|"https://"*|"mailto:"*|"/"*) continue ;;
        esac
        # Strip a #anchor suffix for the existence check.
        path_only="${target%%#*}"
        [[ -z "$path_only" ]] && continue
        if [[ ! -e "${dir}/${path_only}" ]]; then
            printf 'broken link in %s: %s\n' "$file" "$target"
            failures=$((failures + 1))
        fi
    done < <(grep -oP '\[[^]]*\]\(\K[^)]*' "$file" || true)
done < <(git ls-files -z '*.md')

if [[ "$failures" -gt 0 ]]; then
    printf 'check-links: %d broken relative link(s)\n' "$failures" >&2
    exit 1
fi
echo "check-links: all relative links resolve"
