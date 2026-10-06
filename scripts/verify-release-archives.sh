#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/fc-gpui-release-archives.XXXXXX")"
trap 'rm -rf "$TMP_ROOT"' EXIT

# Topological order: also the safe order for sequential `cargo publish`.
readonly RELEASE_PACKAGES=(
    fc-gpui-collections
    fc-gpui-perf
    fc-gpui-util-macros
    fc-gpui-util
    fc-gpui-http-client
    fc-gpui-macros
    fc-gpui-derive-refineable
    fc-gpui-refineable
    fc-gpui-semantic-version
    fc-gpui-sum-tree
    fc-gpui-media
    fc-gpui-core
    fc-gpui-wgpu
    fc-gpui-linux
    fc-gpui-macos
    fc-gpui-windows
    fc-gpui-platform
    fc-gpui
)

cd "$REPO_ROOT"
cargo metadata --locked --no-deps --format-version 1 > "$TMP_ROOT/metadata.json"

python3 - "$TMP_ROOT/metadata.json" "${RELEASE_PACKAGES[@]}" <<'PY' > "$TMP_ROOT/packages.tsv"
import json
import sys

metadata_path, *expected = sys.argv[1:]
with open(metadata_path, encoding="utf-8") as handle:
    packages = {package["name"]: package for package in json.load(handle)["packages"]}

missing = [name for name in expected if name not in packages]
if missing:
    raise SystemExit("missing release packages: " + ", ".join(missing))

for name in expected:
    package = packages[name]
    if package.get("publish") == []:
        raise SystemExit(f"release package is marked publish=false: {name}")
    print(name, package["version"], package["manifest_path"], sep="\t")
PY

mkdir -p "$TMP_ROOT/unpacked" "$TMP_ROOT/.cargo"
: > "$TMP_ROOT/checksums.txt"

package_args=()
for package in "${RELEASE_PACKAGES[@]}"; do
    package_args+=(--package "$package")
done
echo "packaging ${#RELEASE_PACKAGES[@]} workspace release crates together"
cargo package --locked --allow-dirty --no-verify "${package_args[@]}" >/dev/null

while IFS=$'\t' read -r package version _manifest_path; do
    archive="$REPO_ROOT/target/package/$package-$version.crate"
    if [[ ! -f "$archive" ]]; then
        echo "missing archive: $archive" >&2
        exit 1
    fi
    sha256=$(shasum -a 256 "$archive" | awk '{print $1}')
    printf '%s  %s-%s.crate\n' "$sha256" "$package" "$version" >> "$TMP_ROOT/checksums.txt"
    tar -xzf "$archive" -C "$TMP_ROOT/unpacked"
done < "$TMP_ROOT/packages.tsv"

{
    echo '[patch.crates-io]'
    while IFS=$'\t' read -r package version _manifest_path; do
        printf '"%s" = { path = "%s/unpacked/%s-%s" }\n' \
            "$package" "$TMP_ROOT" "$package" "$version"
    done < "$TMP_ROOT/packages.tsv"
} > "$TMP_ROOT/.cargo/config.toml"

while IFS=$'\t' read -r package version _manifest_path; do
    crate_dir="$TMP_ROOT/unpacked/$package-$version"
    normalized_manifest="$crate_dir/Cargo.toml"
    if ! awk '
        /^\[/ { in_dependencies = ($0 ~ /dependencies/) }
        in_dependencies && /^[[:space:]]*path[[:space:]]*=/ { exit 1 }
    ' "$normalized_manifest"; then
        echo "packaged manifest still contains a path dependency: $package" >&2
        exit 1
    fi
    if [[ ! -f "$crate_dir/LICENSE-APACHE" ]]; then
        echo "$package archive is missing LICENSE-APACHE" >&2
        exit 1
    fi
    echo "checking unpacked archive $package $version"
    (
        cd "$TMP_ROOT"
        cargo metadata --format-version 1 --manifest-path "$normalized_manifest" >/dev/null
        cargo check --manifest-path "$normalized_manifest" >/dev/null
    )
done < "$TMP_ROOT/packages.tsv"

sort -k2 "$TMP_ROOT/checksums.txt"
echo "release archive verification passed: ${#RELEASE_PACKAGES[@]} packages"
