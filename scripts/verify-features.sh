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

echo "[1/14] facade: no default features..."
cargo check --locked -q -p fc-gpui --lib --no-default-features

echo "[2/14] facade: accessibility + font-kit + X11..."
cargo check --locked -q -p fc-gpui --lib --no-default-features \
    --features accessibility,font-kit,x11

echo "[3/14] facade: accessibility + font-kit + Wayland..."
cargo check --locked -q -p fc-gpui --lib --no-default-features \
    --features accessibility,font-kit,wayland

echo "[4/14] facade: wgpu headless marker (docs/plans runbook)..."
cargo check --locked -q -p fc-gpui --no-default-features --features wgpu

echo "[5/14] facade: wgpu + Wayland + X11 (docs/sync migration runbook)..."
cargo check --locked -q -p fc-gpui --no-default-features \
    --features wgpu,wayland,x11

echo "[6/14] core: no default features..."
cargo check --locked -q -p fc-gpui-core --no-default-features

echo "[7/14] core: test-support..."
cargo check --locked -q -p fc-gpui-core --features test-support

echo "[8/14] HTTP client: protocol only..."
cargo check --locked -q -p fc-gpui-http-client --lib --no-default-features

echo "[9/14] HTTP client: default reqwest compatibility..."
cargo check --locked -q -p fc-gpui-http-client --lib

echo "[10/14] HTTP client: opt-in macOS system proxy..."
cargo check --locked -q -p fc-gpui-http-client --lib --no-default-features \
    --features macos-system-proxy

echo "[11/14] core: explicit reqwest compatibility..."
cargo check --locked -q -p fc-gpui-core --lib --no-default-features \
    --features http-client-reqwest

echo "[12/14] facade: explicit reqwest compatibility..."
cargo check --locked -q -p fc-gpui --lib --no-default-features \
    --features http-client-reqwest

echo "[13/14] core: protocol-only dependency graph excludes reqwest..."
python3 - <<'PY'
import subprocess

tree = subprocess.run(
    ["cargo", "tree", "--locked", "-p", "fc-gpui-core", "--no-default-features",
     "-e", "normal", "--prefix", "none", "--format", "{p}"],
    check=True, capture_output=True, text=True,
).stdout
if any(line.startswith("reqwest ") for line in tree.splitlines()):
    raise SystemExit("FAIL: protocol-only fc-gpui-core depends on reqwest")
print("protocol-only HTTP dependency contract OK")
PY

echo "[14/14] public feature whitelist..."
python3 - <<'PY'
import json
import subprocess
import sys

EXPECTED = {
    "fc-gpui": [
        "accessibility", "default", "font-kit", "http-client-reqwest", "image-default-formats",
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
        "accessibility", "bench", "default", "font-kit", "http-client-reqwest", "image-default-formats",
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
    "fc-gpui-http-client": ["default", "macos-system-proxy", "reqwest", "test-support"],
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

HTTP_FEATURES = {
    "fc-gpui": ("http-client-reqwest", "gpui_core/http-client-reqwest"),
    "fc-gpui-core": ("http-client-reqwest", "http_client/reqwest"),
    "fc-gpui-http-client": ("reqwest", "dep:reqwest"),
}
for package in meta["packages"]:
    if package["name"] not in HTTP_FEATURES:
        continue
    feature, target = HTTP_FEATURES[package["name"]]
    features = package["features"]
    if feature not in features.get("default", []) or target not in features.get(feature, []):
        failed = True
        print(f"FAIL {package['name']}: default HTTP compatibility forwarding changed")
    if package["name"] == "fc-gpui-http-client":
        reqwest = next(dep for dep in package["dependencies"] if dep["name"] == "reqwest")
        if not reqwest["optional"] or "reqwest" not in features["macos-system-proxy"]:
            failed = True
            print("FAIL fc-gpui-http-client: reqwest must be optional and enabled by macos-system-proxy")

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
