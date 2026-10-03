#!/bin/sh
# Build dist/gantry-<version>-x86_64-linux.tar.gz in an Ubuntu 22.04 container (podman),
# so the binary needs glibc 2.35 and runs on Ubuntu 22.04 and newer.
# PODMAN overrides the podman command (extra options allowed).
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
podman=${PODMAN:-podman}
toolchain=$(sed -n 's/^channel = "\(.*\)"$/\1/p' "$root/rust-toolchain.toml")
$podman build -q -t gantry-build-ubuntu22 --build-arg "RUST_TOOLCHAIN=$toolchain" \
  -f "$root/packaging/linux/ubuntu-22.04.Containerfile" "$root/packaging/linux" >/dev/null
$podman run --rm -v "$root:/src:Z" -v gantry-cargo-ubuntu22:/cargo-home -w /src \
  -e CARGO_HOME=/cargo-home -e CARGO_TARGET_DIR=/src/target/ubuntu-22.04 -e CARGO_BUILD_JOBS=4 \
  gantry-build-ubuntu22 packaging/linux/pack-tarball.sh >/dev/null
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/Cargo.toml" | head -n 1)
echo "$root/dist/gantry-$version-x86_64-linux.tar.gz"
