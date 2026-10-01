#!/usr/bin/env bash
# Re-run a cargo command when crates.io cannot be reached.
#
# GitHub-hosted runners sometimes fail DNS for index.crates.io before
# compilation starts. Cargo's own retries are not always long enough.
# Real compiler and test failures do not match the registry pattern and
# fail on the first attempt.
set -u

if [ "$#" -ne 1 ] || [ -z "$1" ]; then
    echo "usage: ci-retry-registry.sh <command>" >&2
    exit 2
fi

cmd=$1
max=4
attempt=1

while [ "$attempt" -le "$max" ]; do
    log=$(mktemp)
    bash -c "$cmd" 2>&1 | tee "$log"
    status=${PIPESTATUS[0]}
    if [ "$status" -eq 0 ]; then
        rm -f "$log"
        exit 0
    fi
    if ! grep -E -q "Couldn't resolve host|Could not resolve host|failed to load source for dependency|unable to update registry|download of config.json failed|spurious network error" "$log"; then
        rm -f "$log"
        exit "$status"
    fi
    rm -f "$log"
    if [ "$attempt" -eq "$max" ]; then
        exit "$status"
    fi
    delay=$((attempt * 20))
    echo "crates.io was unreachable (attempt ${attempt}/${max}); retrying in ${delay}s" >&2
    sleep "$delay"
    attempt=$((attempt + 1))
done
