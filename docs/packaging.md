# Arch packaging and installation verification

## Build a local test package

Run these commands from the repository root on Arch Linux:

```sh
build_dir=$(mktemp -d "${TMPDIR:?}/shisa-arch.XXXXXXXX")
packaging/arch/prepare-build.sh --worktree "$build_dir"
(cd "$build_dir" && SRCDEST="$build_dir" makepkg)
```

The source archive includes modified tracked files and non-ignored additions. Deleted files and `packaging/` are excluded. The script stages its own checksum and sets a decimal test `pkgrel` in the copied recipe. The repository's `PKGBUILD` stays unchanged. Install needed build dependencies separately if they are missing. Both the source archive and the generated package stay in the build directory.

A later formal package with the same base `pkgrel` sorts below the local decimal test build. Install the formal package explicitly with `pacman -U` if switching back from the test build.

## GitHub Release packages

The [Arch package workflow](../.github/workflows/arch-package.yml) builds the commit selected by a published `vX.Y.Z` Release. The tag must match the tagged `PKGBUILD` version. The build job runs `makepkg` and `check()` in a fresh Arch Linux image and checks the installed file list. `check()` runs the Rust workspace tests, the settings window's Node and vitest suites, its Tauri tests, and the KWin script test. The upload job attaches the x86_64 package and `SHA256SUMS` to that same Release. It preserves existing assets and reports a failure if an existing asset differs from the build.

Use **Actions → Build Arch package → Run workflow** to build an Actions artifact from a selected branch or tag. A manual run builds a test package and does not upload Release assets. The workflow assumes release assets can be added after publication.

On 2026-11-21, the v0.1.1 release build failed twice in the container: one test required a systemd user manager to export `WAYLAND_DISPLAY`, and another resolved `/usr/bin/flatpak` from the host. Both tests no longer depend on the host, and the rebuilt release attached `shisa-0.1.1-1-x86_64.pkg.tar.zst` and `SHA256SUMS`.

For manual AUR preparation, `packaging/arch/release.sh` still creates the tagged source archive, updates the hash, and writes `.SRCINFO`. That source archive can be uploaded separately when needed.

## Installation and upgrade verification

With approval, install the generated package using `pacman -U`. After upgrading, restart `shisad.service` and the user unit `shisa-agent.service` through the same approval process, then reopen the settings window. Set a separate app-lock password on first install as shown in the [README](../README.md#installation). Inspect the installed files with `pacman -Qkk shisa` and follow [Tests and verification](testing.md) for the affected behavior.

On 2026-11-17, upgrading a temporary Arch package replaced a protected file and the post-transaction hook marked the new inode before the 30-second polling interval elapsed. On 2026-11-18, the package built with `extra-x86_64-build` in a clean chroot.
