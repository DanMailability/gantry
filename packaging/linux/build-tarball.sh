#!/bin/sh
# Build the release binary and pack dist/gantry-<version>-x86_64-linux.tar.gz.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/Cargo.toml" | head -n 1)
cd "$root"
cargo build --release -p gantry
stage=$root/dist/gantry-$version
rm -rf "$stage"
mkdir -p "$stage"
install -m0755 target/release/gantry packaging/linux/install.sh "$stage/"
install -m0644 packaging/linux/io.github.idogrf.Gantry.desktop packaging/linux/io.github.idogrf.Gantry.svg \
  LICENSE-MIT LICENSE-APACHE "$stage/"
tar -C "$root/dist" -czf "$root/dist/gantry-$version-x86_64-linux.tar.gz" "gantry-$version"
echo "$root/dist/gantry-$version-x86_64-linux.tar.gz"
