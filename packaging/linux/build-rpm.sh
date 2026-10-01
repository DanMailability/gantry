#!/bin/sh
# Build dist/rpmbuild/RPMS/x86_64/gantry-*.rpm from the release tarball.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
tarball=$(sh "$root/packaging/linux/build-tarball.sh" | tail -n 1)
top=$root/dist/rpmbuild
mkdir -p "$top/SOURCES"
cp "$tarball" "$top/SOURCES/"
rpmbuild -bb --define "_topdir $top" "$root/packaging/linux/gantry.spec"
find "$top/RPMS" -name 'gantry-*.rpm'
