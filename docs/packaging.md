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

On 2026-11-21, v0.1.2 built and passed `check()` in the release container. The release attached `shisa-0.1.2-1-x86_64.pkg.tar.zst`, `SHA256SUMS` and `shisa-0.1.2.tar.gz`. The downloaded package and source archive matched their checksums, and the package lists the four binaries, both systemd units and the polkit action and rule.

For manual AUR preparation, `packaging/arch/release.sh` still creates the tagged source archive, updates the hash, and writes `.SRCINFO`. That source archive can be uploaded separately when needed.

## Installation and upgrade verification

With approval, install the generated package using `pacman -U`. After upgrading, restart `shisad.service` and the user unit `shisa-agent.service` through the same approval process, then reopen the settings window. Set a separate app-lock password on first install as shown in the [README](../README.md#installation). Inspect the installed files with `pacman -Qkk shisa` and follow [Tests and verification](testing.md) for the affected behavior.

On 2026-11-17, upgrading a temporary Arch package replaced a protected file and the post-transaction hook marked the new inode before the 30-second polling interval elapsed. On 2026-11-18, the package built with `extra-x86_64-build` in a clean chroot.

On 2026-11-22, four local test packages `0.1.2-1.1` were built from the worktree and three were installed over `0.1.2-1` with `pacman -U`. Each install left `pacman -Qkk shisa` with no changed files. The first restarted both services and confirmed them active, and the later two replaced only the settings frontend. The settings window reopened from `/usr/bin/shisa-settings` after clearing `$XDG_RUNTIME_DIR/shisa-settings`, and its built assets were checked for the new sidebar icons, the trimmed copy and the `system-ui` font stack. The `inter-font` optdepend was removed at the same time.

On 2026-11-22, v0.1.3 built and passed `check()` in the release container. The release attached `shisa-0.1.3-1-x86_64.pkg.tar.zst`, `SHA256SUMS` and `shisa-0.1.3.tar.gz`. The downloaded package and source archive matched their checksums.

On 2026-11-22, a local test package `shisa-0.1.3-1.1` was built from the worktree with `prepare-build.sh --worktree`. `makepkg`'s `check()` passed the workspace tests, the settings frontend and Tauri tests, and the KWin script tests, and `pacman -Qkp` found every file. It was installed over `0.1.2-1.1` with `pacman -U`. `pacman -Qkk shisa` reported 0 changed files, both services restarted at 11:27:29, and `Diagnostics` reported version 0.1.3. The live Wayland verification of restoring a focus-loss-hidden window inside and outside its grace period is still pending.

On 2026-11-22, a second local test package `shisa-0.1.3-1.2` added the app-tray restore path. `makepkg`'s `check()` passed, including the KWin script tests for a hidden app activation that asks `RestoreIfTicketed` without showing the window first. It was installed over `0.1.3-1.1` with `pacman -U` and `pacman -Qkk shisa` reported 0 changed files. Only `shisa-agent.service` was restarted, so `shisad` kept its PID and the existing tickets and window permissions. The active script in `/run/user/1000/shisa` contains `RestoreIfTicketed`. The user verified the app-tray restore path in the live Wayland session.
