#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd "$(dirname "$0")/.." && pwd)
cd "$repo_dir"

package_version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)
bundle_version=${package_version%%-*}
build_dir="$repo_dir/target/macos"
swift_build_dir="$repo_dir/target/swift"
app_dir="$build_dir/RDRKit.app"
contents_dir="$app_dir/Contents"

cargo build --release --locked
swift build \
    --package-path macos/RDRKitApp \
    --configuration release \
    --scratch-path "$swift_build_dir"

rm -rf "$app_dir"
mkdir -p "$contents_dir/MacOS" "$contents_dir/Resources"
cp macos/RDRKitApp/Resources/Info.plist "$contents_dir/Info.plist"
cp "$swift_build_dir/release/RDRKitApp" "$contents_dir/MacOS/RDRKitApp"
cp target/release/rdrkit "$contents_dir/MacOS/rdrkit"
chmod 0755 "$contents_dir/MacOS/RDRKitApp" "$contents_dir/MacOS/rdrkit"

/usr/libexec/PlistBuddy \
    -c "Set :CFBundleShortVersionString $bundle_version" \
    -c "Set :CFBundleVersion 1" \
    "$contents_dir/Info.plist"
plutil -lint "$contents_dir/Info.plist"

codesign_identity=${RDRKIT_CODESIGN_IDENTITY:--}
codesign --force --sign "$codesign_identity" "$contents_dir/MacOS/rdrkit"
codesign --force --sign "$codesign_identity" --deep "$app_dir"
codesign --verify --deep --strict "$app_dir"

echo "$app_dir"
