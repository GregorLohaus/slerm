#!/usr/bin/env bash
# Wraps a release build in slerm.app, ad-hoc signs it, and zips it.
#
#   cargo build --release
#   scripts/bundle-macos.sh [version]
#
# Output: target/bundle/slerm.app and target/bundle/slerm-<version>-macos-<arch>.zip
# The Info.plist mirrors the one in flake.nix; keep the two in sync.
set -euo pipefail

cd "$(dirname "$0")/.."

version="${1:-$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)}"
arch="$(uname -m)"
out="target/bundle"
app="$out/slerm.app"

rm -rf "$out"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"

cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>slerm</string>
  <key>CFBundleDisplayName</key><string>slerm</string>
  <key>CFBundleExecutable</key><string>slerm</string>
  <key>CFBundleIconFile</key><string>slerm.icns</string>
  <key>CFBundleIdentifier</key><string>org.slerm.slerm</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${version}</string>
  <key>CFBundleVersion</key><string>${version}</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSMicrophoneUsageDescription</key><string>Programs running in slerm, such as voice input tools, want to use the microphone.</string>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
EOF

cp assets/slerm.icns "$app/Contents/Resources/slerm.icns"
cp target/release/slerm "$app/Contents/MacOS/slerm"

# Apple Silicon refuses unsigned code; an ad-hoc signature is enough to launch.
codesign --force --deep --sign - "$app"

zip="slerm-${version}-macos-${arch}.zip"
(cd "$out" && ditto -c -k --keepParent slerm.app "$zip" && shasum -a 256 "$zip" > "$zip.sha256")
echo "Built $out/$zip"
