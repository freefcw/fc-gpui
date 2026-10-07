#!/usr/bin/env bash
# Public feature contract checks for the published GPUI crates.
#
# Part 1 compiles the feature combinations used by CI and by the runbooks in
# docs/plans and docs/sync, so renaming or removing a public feature fails here
# instead of silently breaking those runbooks.
#
# Part 2 pins each published crate's public feature list against a whitelist.
# Any addition or removal shows up as an explicit failure and must be
# acknowledged by updating the whitelist in the same commit. This is what keeps
# implementation-dependency names (e.g. `ashpd`, `backtrace`, `git2`) from
# leaking back into the public API as no-op features.
set -euo pipefail

echo "=== Feature contracts ==="

echo "[1/8] facade: no default features..."
cargo check --locked -q -p fc-gpui --lib --no-default-features

echo "[2/8] facade: accessibility + font-kit + X11..."
cargo check --locked -q -p fc-gpui --lib --no-default-features \
    --features accessibility,font-kit,x11

echo "[3/8] facade: accessibility + font-kit + Wayland..."
cargo check --locked -q -p fc-gpui --lib --no-default-features \
    --features accessibility,font-kit,wayland

echo "[4/8] facade: wgpu headless marker (docs/plans runbook)..."
cargo check --locked -q -p fc-gpui --no-default-features --features wgpu

echo "[5/8] facade: wgpu + Wayland + X11 (docs/sync migration runbook)..."
cargo check --locked -q -p fc-gpui --no-default-features \
    --features wgpu,wayland,x11

echo "[6/8] core: no default features..."
cargo check --locked -q -p fc-gpui-core --no-default-features

echo "[7/8] core: test-support..."
cargo check --locked -q -p fc-gpui-core --features test-support

echo "[8/8] public feature whitelist..."
python3 - <<'PY'
import json
import subprocess
import sys

EXPECTED = {
    "fc-gpui": [
        "accessibility", "default", "font-kit", "image-default-formats",
        "image-format-avif", "image-format-bmp", "image-format-dds",
        "image-format-exr", "image-format-farbfeld", "image-format-gif",
        "image-format-hdr", "image-format-ico", "image-format-jpeg",
        "image-format-png", "image-format-pnm", "image-format-qoi",
        "image-format-tga", "image-format-tiff", "image-format-webp",
        "image-rayon", "input-latency-histogram", "inspector", "keyring",
        "leak-detection", "runtime_shaders", "screen-capture", "test-support",
        "wayland", "wgpu", "windows-manifest", "x11",
    ],
    "fc-gpui-core": [
        "accessibility", "bench", "default", "font-kit", "image-default-formats",
        "image-format-avif", "image-format-bmp", "image-format-dds",
        "image-format-exr", "image-format-farbfeld", "image-format-gif",
        "image-format-hdr", "image-format-ico", "image-format-jpeg",
        "image-format-png", "image-format-pnm", "image-format-qoi",
        "image-format-tga", "image-format-tiff", "image-format-webp",
        "image-rayon", "input-latency-histogram", "inspector",
        "leak-detection", "runtime_shaders", "screen-capture", "test-support",
        "wayland", "windows-manifest", "x11",
    ],
    "fc-gpui-platform": [
        "accessibility", "default", "font-kit", "image-default-formats",
        "image-format-avif", "image-format-bmp", "image-format-dds",
        "image-format-exr", "image-format-farbfeld", "image-format-gif",
        "image-format-hdr", "image-format-ico", "image-format-jpeg",
        "image-format-png", "image-format-pnm", "image-format-qoi",
        "image-format-tga", "image-format-tiff", "image-format-webp",
        "image-rayon", "input-latency-histogram", "inspector", "keyring",
        "leak-detection", "runtime_shaders", "screen-capture", "test-support",
        "wayland", "wgpu", "windows-manifest", "x11",
    ],
    "fc-gpui-linux": [
        "accessibility", "default", "font-kit", "keyring", "screen-capture",
        "test-support", "wayland", "x11",
    ],
    "fc-gpui-macos": [
        "accessibility", "default", "font-kit", "runtime_shaders",
        "screen-capture", "test-support",
    ],
    "fc-gpui-windows": [
        "accessibility", "default", "screen-capture", "test-support",
        "windows-manifest",
    ],
    "fc-gpui-wgpu": ["default", "test-support"],
    "fc-gpui-util": ["test-support"],
    "fc-gpui-util-macros": ["perf-enabled"],
}

meta = json.loads(
    subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
)

failed = False
for package in meta["packages"]:
    name = package["name"]
    if name not in EXPECTED:
        continue
    actual = set(package["features"])
    expected = set(EXPECTED[name])
    if actual == expected:
        continue
    failed = True
    print(f"FAIL {name}:")
    added = sorted(actual - expected)
    removed = sorted(expected - actual)
    if added:
        print(f"  unexpected features: {added}")
    if removed:
        print(f"  missing features:    {removed}")

for package in meta["packages"]:
    if package["name"] != "fc-gpui-macos":
        continue
    deps = {d["name"]: d for d in package["dependencies"]}
    screen_capture = package["features"].get("screen-capture", [])
    violations = []
    for dep_name in ("objc2-core-media", "objc2-screen-capture-kit"):
        dep = deps.get(dep_name)
        if dep is None or not dep["optional"]:
            violations.append(f"{dep_name} is not an optional dependency")
        if f"dep:{dep_name}" not in screen_capture:
            violations.append(f"screen-capture does not enable dep:{dep_name}")
    for dep_name in ("objc2-app-kit", "objc2-foundation"):
        dep = deps.get(dep_name)
        if dep is None or dep["uses_default_features"]:
            violations.append(f"{dep_name} does not set default-features = false")
    if violations:
        failed = True
        print("FAIL fc-gpui-macos:")
        for violation in violations:
            print(f"  {violation}")

if failed:
    print()
    print("Public feature lists changed. If intentional, update EXPECTED in")
    print("scripts/verify-features.sh and note it in CHANGELOG.md in the same")
    print("commit. Otherwise restore the previous feature list.")
    sys.exit(1)

print("feature whitelist OK")
PY

echo "=== Feature contracts ALL PASSED ==="
