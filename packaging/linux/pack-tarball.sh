#!/bin/sh
# Build the release binary and pack dist/gantry-<version>-x86_64-linux.tar.gz (run by build-tarball.sh inside its container).
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/Cargo.toml" | head -n 1)
cd "$root"
cargo build --release -p gantry
stage=$root/dist/gantry-$version
rm -rf "$stage"
mkdir -p "$stage"
install -m0755 "${CARGO_TARGET_DIR:-target}/release/gantry" packaging/linux/install.sh "$stage/"
install -m0644 packaging/linux/io.github.idogrf.Gantry.desktop packaging/linux/io.github.idogrf.Gantry.svg \
  LICENSE-MIT LICENSE-APACHE "$stage/"
# The embedded interface font's licence.
install -m0644 crates/ui-gpui/assets/fonts/OFL.txt "$stage/LICENSE-FONT-FIGTREE"
tar -C "$root/dist" -czf "$root/dist/gantry-$version-x86_64-linux.tar.gz" "gantry-$version"
echo "$root/dist/gantry-$version-x86_64-linux.tar.gz"
