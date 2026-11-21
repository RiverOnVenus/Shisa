#!/bin/sh
# Prepare an AUR update without publishing anything.
#
#   ./release.sh
#
# Requires the release tag `v$pkgver`. Builds the source tarball from it,
# writes its sha256 into PKGBUILD and writes .SRCINFO. It does not touch git
# and does not upload. After this, upload the printed tarball to the release
# URL, then commit the PKGBUILD.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
pkgname=$(sed -n 's/^pkgname=//p' "$here/PKGBUILD")
version=$(sed -n 's/^pkgver=//p' "$here/PKGBUILD")
url=$(sed -n 's/^url="\(.*\)"/\1/p' "$here/PKGBUILD")
[ -n "$url" ] || { echo 'PKGBUILD has no url="..."' >&2; exit 1; }

tarball=$("$here/make-source.sh")
sum=$(sha256sum "$tarball" | cut -d' ' -f1)

# The checksum has to be in PKGBUILD before .SRCINFO is written.
sed -i "s|^sha256sums=.*|sha256sums=('$sum')|" "$here/PKGBUILD"

printf 'source:  %s/releases/download/v%s/%s-%s.tar.gz\nsha256:  %s\ntarball: %s\n' \
    "$url" "$version" "$pkgname" "$version" "$sum" "$tarball"
printf 'upload the tarball to that URL before makepkg can download it\n'

cd "$here" && makepkg --printsrcinfo > .SRCINFO
printf '.SRCINFO written, %s lines\n' "$(wc -l < .SRCINFO)"

cd "$repo" && git status --short | head -5
