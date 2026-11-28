#!/bin/sh
# Fake journalctl for the agent tests. Run through a symlink in a per-test
# directory so it never has to be written (a file open for writing makes
# exec fail with ETXTBSY when another thread forks). Argv goes to args
# beside the symlink.
printf '%s\n' "$@" > "$(dirname "$0")/args"
echo '{"__CURSOR":"c1","MESSAGE":"one"}'
echo '{"__CURSOR":"c2","MESSAGE":"two"}'
