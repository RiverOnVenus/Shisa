#!/bin/sh
# Build the source tarball the PKGBUILD expects from the release tag.
#
# The archive comes from `v$version`, not the working tree, so the published
# tarball always matches the tag. packaging/ is left out so packaging changes
# do not alter the tarball or its checksum.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
version=$(sed -n 's/^pkgver=//p' "$here/PKGBUILD")
out="$here/shisa-$version.tar.gz"

git -C "$repo" rev-parse --verify --quiet "refs/tags/v$version" >/dev/null || {
    echo "tag v$version does not exist, tag the release commit first" >&2
    exit 1
}

# A release install compares the settings app's own crate version with
# shisad's. Every manifest must name the release version, or a correct
# install shows a permanent mismatch notice. Read the tag, not the working
# tree, because the tag is what goes into the tarball.
check_version() {
    found=$(git -C "$repo" show "v$version:$1" 2>/dev/null | sed -n "$2")
    if [ "$found" != "$version" ]; then
        echo "$1 in tag v$version names version '${found:-<none>}', expected $version" >&2
        exit 1
    fi
}
check_version Cargo.toml 's/^version = "\(.*\)"/\1/p'
check_version gui/shisa-settings/src-tauri/Cargo.toml 's/^version = "\(.*\)"/\1/p'
check_version gui/shisa-settings/package.json 's/^  "version": "\(.*\)",/\1/p'
check_version gui/shisa-settings/src-tauri/tauri.conf.json 's/^  "version": "\(.*\)",/\1/p'

# The tarball is taken from the tag. Warn when the working tree differs from
# it outside packaging, since those changes will not be in the tarball.
if ! git -C "$repo" diff --quiet "v$version" -- . ':(exclude)packaging'; then
    echo "warning: source changes outside packaging are not in tag v$version" >&2
    echo "         and will not be in the tarball" >&2
fi

git -C "$repo" archive --format=tar.gz --prefix="shisa-$version/" \
    --output="$out" "v$version" -- . ':(exclude)packaging'

echo "$out"
