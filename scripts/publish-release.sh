#!/usr/bin/env bash
# Publish the workspace release crates to crates.io in topological order.
#
# Crate versions that already exist on crates.io are skipped, so the script
# is safe to re-run after a partial failure. Set DRY_RUN=1 to package and
# verify each crate without uploading.
#
# Requires CARGO_REGISTRY_TOKEN in the environment (unless DRY_RUN=1).
set -euo pipefail

DRY_RUN="${DRY_RUN:-0}"

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

# Topological publish order, shared with verify-release-archives.sh.
source "$SCRIPT_DIR/release-packages.sh"

readonly USER_AGENT="fc-gpui-release (https://github.com/freefcw/fc-gpui)"

if [[ "$DRY_RUN" == "1" ]]; then
    # `cargo publish --dry-run` still resolves normalized path dependencies
    # from crates.io, so it cannot validate a fresh lockstep release before
    # those dependencies have been uploaded. Verify the packaged archives
    # against one another locally instead.
    "$SCRIPT_DIR/verify-release-archives.sh"
    echo "dry run finished: ${#RELEASE_PACKAGES[@]} crates packaged and verified"
    exit 0
fi

# Sparse index path for a crate name (https://doc.rust-lang.org/cargo/reference/registries.html).
index_path() {
    local name="$1"
    case "${#name}" in
        1) echo "1/$name" ;;
        2) echo "2/$name" ;;
        3) echo "3/${name:0:1}/$name" ;;
        *) echo "${name:0:2}/${name:2:2}/$name" ;;
    esac
}

version_published() {
    local name="$1" version="$2"
    curl -sf -o /dev/null -H "User-Agent: $USER_AGENT" \
        "https://crates.io/api/v1/crates/$name/$version"
}

# Wait until the sparse index (what cargo resolves against) lists the version.
wait_for_index() {
    local name="$1" version="$2"
    local url="https://index.crates.io/$(index_path "$name")"
    local attempt
    for attempt in $(seq 1 30); do
        if curl -sf -H "User-Agent: $USER_AGENT" "$url" | grep -F "\"vers\":\"$version\"" >/dev/null; then
            echo "index lists $name $version after ${attempt} check(s)"
            return 0
        fi
        sleep 5
    done
    echo "timed out waiting for $name $version in the sparse index" >&2
    return 1
}

publish_one() {
    local name="$1"
    local -a args=(publish --package "$name" --no-verify --locked)

    local attempt log
    log="$(mktemp)"
    for attempt in 1 2 3 4 5; do
        if cargo "${args[@]}" >"$log" 2>&1; then
            rm -f "$log"
            return 0
        fi
        # A previous attempt (or concurrent run) may have completed the upload.
        if grep -qiE 'already (exists|uploaded|published)' "$log"; then
            echo "$name appears to be published already; continuing"
            rm -f "$log"
            return 0
        fi
        echo "cargo publish failed for $name (attempt $attempt/5); retrying in $((30 * attempt))s" >&2
        cat "$log" >&2
        sleep $((30 * attempt))
    done
    echo "giving up on $name" >&2
    rm -f "$log"
    return 1
}

metadata="$(cargo metadata --locked --no-deps --format-version 1)"
published=()
skipped=()

for name in "${RELEASE_PACKAGES[@]}"; do
    version="$(METADATA="$metadata" python3 - "$name" <<'PY'
import json
import os
import sys

packages = json.loads(os.environ["METADATA"])["packages"]
by_name = {package["name"]: package for package in packages}
print(by_name[sys.argv[1]]["version"])
PY
)"

    if version_published "$name" "$version"; then
        echo ":: $name $version is already on crates.io; skipping"
        # A previous run may have uploaded the crate but timed out while
        # waiting for sparse-index propagation. Do not let a dependent crate
        # race that still-in-flight publication on recovery.
        wait_for_index "$name" "$version"
        skipped+=("$name $version")
        continue
    fi

    echo ":: publishing $name $version"
    publish_one "$name"

    # Dependents resolve against the sparse index; wait before continuing.
    wait_for_index "$name" "$version"
    published+=("$name $version")
done

echo
echo "published: ${#published[@]} crate(s)"
if ((${#published[@]})); then printf '  %s\n' "${published[@]}"; fi
echo "skipped (already on crates.io): ${#skipped[@]} crate(s)"
if ((${#skipped[@]})); then printf '  %s\n' "${skipped[@]}"; fi
