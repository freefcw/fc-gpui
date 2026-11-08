#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
AUDIT_FILE="${UPSTREAM_AUDIT_FILE:-$REPO_ROOT/docs/sync/upstream-audit.json}"
UPSTREAM_REPO="${UPSTREAM_REPO:-$REPO_ROOT/../zed}"

usage() {
    cat <<'EOF'
Usage: scripts/verify-upstream-sync.sh [--upstream PATH] [--audit PATH]

Validates that every mapped upstream commit in the audited range has a
classification and that every backport (including supplemental and
post-audit cherry-picks) has an exact Zed-Origin trailer in the local Git
history. Optional compared_against / newest_ported_* fields are checked when
present: newest_ported_by_date must be the traced backport with the latest
upstream committer date (tie-break: larger SHA); newest_ported_by_trailer
must be the traced backport whose matching local commit is latest by
committer date (tie-break: larger local SHA, then larger origin SHA).
This command is read-only and never fetches the network.
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --upstream)
            UPSTREAM_REPO="$2"
            shift 2
            ;;
        --audit)
            AUDIT_FILE="$2"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            usage >&2
            exit 2
            ;;
    esac
done

python3 "$SCRIPT_DIR/lib/upstream_sync_audit.py" "$REPO_ROOT" "$UPSTREAM_REPO" "$AUDIT_FILE"
