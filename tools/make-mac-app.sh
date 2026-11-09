#!/usr/bin/env bash
# Builds Cullery.app and a Cullery-Mac.dmg to install it from, in target/mac-app.
# Run on a Mac: bash tools/make-mac-app.sh
set -euo pipefail
cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
cargo build --release

out=target/mac-app
app="$out/Cullery.app"
rm -rf "$out"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/cullery "$app/Contents/MacOS/cullery"

# The icon, in every size macOS uses.
iconset="$out/cullery.iconset"
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" assets/icon.png --out "$iconset/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    sips -z "$double" "$double" assets/icon.png --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/cullery.icns"

cat >"$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>Cullery</string>
    <key>CFBundleDisplayName</key>
    <string>Cullery</string>
    <key>CFBundleIdentifier</key>
    <string>com.nuisancecs.cullery</string>
    <key>CFBundleExecutable</key>
    <string>cullery</string>
    <key>CFBundleIconFile</key>
    <string>cullery</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleVersion</key>
    <string>$version</string>
    <key>CFBundleShortVersionString</key>
    <string>$version</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSRemovableVolumesUsageDescription</key>
    <string>Cullery opens photos from memory cards and external drives.</string>
</dict>
</plist>
EOF

# Not an Apple Developer signature, but Apple Silicon Macs won't run an app with none.
codesign --force --deep --sign - "$app"

# The disk image: the app beside a link to Applications, to drag it onto.
stage="$out/dmg"
mkdir -p "$stage"
cp -R "$app" "$stage/"
ln -s /Applications "$stage/Applications"
hdiutil create -volname Cullery -srcfolder "$stage" -format UDZO -ov "$out/Cullery-Mac.dmg"

echo "Built $out/Cullery-Mac.dmg (version $version)"
