# Arch packaging and installation verification

## Build a test package

Run these commands from a maintainer checkout with the separately maintained `packaging/arch/` files. The scripts require the repository root and the `v$pkgver` tag. Check that the tag points to the commit you intend to verify.

```sh
cd packaging/arch
./release.sh
SRCDEST="$PWD" makepkg -f
```

`release.sh` calls `make-source.sh` to archive the tagged sources, updates the checksum in `PKGBUILD`, and writes `.SRCINFO`. `SRCDEST="$PWD"` lets makepkg use the local archive. After publication, repeat the build using the release download URL to verify the published archive. The maintainer checkout has publishing details in `packaging/arch/packaging-notes.md`.

## Installation and upgrade verification

With approval, install the generated package using `pacman -U`. After upgrading, restart `shisad.service` and the user unit `shisa-agent.service` through the same approval process, then reopen the settings window. Set a separate app-lock password on first install as shown in the [README](../README.md#installation). Inspect the installed files with `pacman -Qkk shisa` and follow [Tests and verification](testing.md) for the affected behavior.

On 2026-11-17, upgrading a temporary Arch package replaced a protected file and the post-transaction hook marked the new inode before the 30-second polling interval elapsed. On 2026-11-18, the package built with `extra-x86_64-build` in a clean chroot.
