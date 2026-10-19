#!/bin/sh
# Regenerates THIRD_PARTY_LICENSES.html from Cargo.lock. Needs cargo-about (cargo install --locked cargo-about --features cli).
set -eu
cd "$(dirname "$0")/.."
cargo about generate about.hbs > THIRD_PARTY_LICENSES.html
