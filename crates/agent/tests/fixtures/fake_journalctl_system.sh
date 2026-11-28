#!/bin/sh
# Fake journalctl for the system-unit log test; see fake_journalctl_site.sh.
printf '%s\n' "$@" > "$(dirname "$0")/args"
echo '{"__CURSOR":"c1","MESSAGE":"\u001b[32mcaddy up"}'
