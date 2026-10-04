#!/bin/sh
# Build the release binary and pack dist/gantry-<version>-aarch64-macos.dmg (run on an Apple Silicon Mac, by hand or by .github/workflows/release.yml).
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/Cargo.toml" | head -n 1)
cd "$root"
cargo build --release -p gantry
app=$root/dist/Gantry.app
rm -rf "$app" "$root/dist/dmg"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" "$root/dist/dmg"
install -m0755 target/release/gantry "$app/Contents/MacOS/gantry"
sed "s/@VERSION@/$version/" packaging/macos/Info.plist > "$app/Contents/Info.plist"
# icon-1024.png is rendered from the Linux SVG; iconutil wants every size up to 1024 (@2x = double).
iconset=$root/dist/Gantry.iconset
rm -rf "$iconset"
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
  sips -z $size $size packaging/macos/icon-1024.png --out "$iconset/icon_${size}x${size}.png" > /dev/null
  sips -z $((size * 2)) $((size * 2)) packaging/macos/icon-1024.png --out "$iconset/icon_${size}x${size}@2x.png" > /dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/Gantry.icns"
rm -rf "$iconset"
mkdir -p "$app/Contents/Resources/licenses"
cp LICENSE-MIT LICENSE-APACHE "$app/Contents/Resources/licenses/"
cp crates/ui-gpui/assets/fonts/OFL.txt "$app/Contents/Resources/licenses/LICENSE-FONT-FIGTREE"
# Ad-hoc signature (no account, no certificate): Apple Silicon refuses to run an unsigned bundle.
codesign --force --deep --sign - "$app"
cp -R "$app" "$root/dist/dmg/"
ln -s /Applications "$root/dist/dmg/Applications"
dmg=$root/dist/gantry-$version-aarch64-macos.dmg
rm -f "$dmg"
hdiutil create -volname Gantry -srcfolder "$root/dist/dmg" -ov -format UDZO "$dmg" > /dev/null
rm -rf "$root/dist/dmg"
echo "$dmg"
