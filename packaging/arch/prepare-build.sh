#!/usr/bin/env bash
# Stage an isolated makepkg input directory from a release tag or the worktree.
set -euo pipefail

usage() {
    echo "Usage: ${0##*/} --release <tag> <empty-directory> | --worktree <empty-directory>" >&2
    exit 2
}

here=$(cd -- "$(dirname -- "$0")" && pwd)
repo=$(cd -- "$here/../.." && pwd -P)
version=$(sed -n 's/^pkgver=//p' "$here/PKGBUILD")
release=$(sed -n 's/^pkgrel=//p' "$here/PKGBUILD")

case "${1:-}" in
    --release)
        [[ $# -eq 3 ]] || usage
        [[ $2 == "v$version" ]] || {
            echo "Release tag $2 must match PKGBUILD version v$version" >&2
            exit 1
        }
        ref="refs/tags/$2"
        commit=$(git -C "$repo" rev-parse --verify "$ref^{commit}")
        [[ $commit == "$(git -C "$repo" rev-parse HEAD)" ]] || {
            echo "Check out the release tag before preparing a release build" >&2
            exit 1
        }
        out=$3
        ;;
    --worktree)
        [[ $# -eq 2 ]] || usage
        out=$2
        ;;
    *) usage ;;
esac

mkdir -p -- "$out"
out=$(cd -- "$out" && pwd -P)
if [[ -n $(find "$out" -mindepth 1 -maxdepth 1 -print -quit) ]]; then
    echo "Build input directory must be empty: $out" >&2
    exit 1
fi

cp -- "$here/PKGBUILD" "$here/shisa.install" "$out/"
archive="$out/shisa-$version.tar.gz"
if [[ $1 == --release ]]; then
    git -C "$repo" archive --format=tar.gz --prefix="shisa-$version/" \
        --output="$archive" "$commit" -- . ':(exclude)packaging'
else
    # Include non-ignored additions and modified tracked files, and omit
    # tracked files removed from the worktree.
    (
        cd "$repo"
        git ls-files -z --cached --others --exclude-standard --deduplicate -- . ':(exclude)packaging'
    ) | (
        cd "$repo"
        while IFS= read -r -d '' path; do
            if [[ "$repo/$path" != "$out/"* && ( -f $path || -L $path ) ]]; then
                printf '%s\0' "$path"
            fi
        done
    ) | tar -C "$repo" --no-recursion --null --files-from=- \
        --transform="flags=rh;s|^|shisa-$version/|" -czf "$archive"
    # Arch pkgrel accepts a decimal subrelease. Keep the canonical recipe intact.
    sed -i "s/^pkgrel=.*/pkgrel=${release%%.*}.1/" "$out/PKGBUILD"
fi
sum=$(sha256sum "$archive")
sed -i "s/^sha256sums=.*/sha256sums=('${sum%% *}')/" "$out/PKGBUILD"
printf '%s\n' "$out"
