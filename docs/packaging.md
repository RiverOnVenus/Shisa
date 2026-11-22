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

On 2026-11-22, a new local test package `shisa-0.1.3-1.1` was built from the worktree with the policy-mode, scan-budget, and test-harness changes. `makepkg`'s `check()` passed the workspace tests, the settings frontend and Tauri tests, and the KWin script tests. `pacman -Qip` reports the install script, and `bsdtar` shows the four binaries, both units, the polkit action and rule, the D-Bus policy, the sysusers entry, the pacman hook, `ConfigurationDirectoryMode=0700` in `shisad.service`, and `secure_config` in `.INSTALL`. The package was not installed.

On 2026-11-22, a local test package `shisa-0.1.3-1.1` was built from the `audit/small-hardening` worktree. `makepkg`'s `check()` passed the workspace tests, the settings frontend and Tauri tests, and the KWin script tests. `pacman -Qip` reports the install script, and the package carries the `secure_config` migration in `.INSTALL` and `ConfigurationDirectoryMode=0700` in `shisad.service`. The package was not installed.

On 2026-11-22, the local test package `shisa-0.1.3-1.1` was installed over the installed 0.1.3-1.1 with `pacman -U`. `pacman -Qkk shisa` reported 0 changed files out of 49. `post_upgrade` tightened `/etc/shisa/users` to 0700 and its files to 0600. `shisad.service` and `shisa-agent.service` were restarted, both active, and `Diagnostics` reported 0.1.3. The live KWin checks are pending.

## Dependency advisories

On 2026-11-22, the two open Dependabot alerts were reviewed.

- `atty` GHSA-g98v-hv3f-hcfr / RUSTSEC-2021-0145 entered only as a build dependency of `ksni 0.2.2`: `dbus-codegen 0.9.1` and `clap 2.34.0` pulled in `atty 0.2.14`. The agent now uses `ksni 0.3.7`, which speaks zbus directly. `cargo tree -i atty` reports that no package matches, and `Cargo.lock` contains neither `atty` nor `dbus-codegen` nor `clap 2`. The advisory is Windows-only and build-time, so the agent binary's runtime behavior is unchanged.
- `glib` GHSA-wrw7-89jp-8q8g / RUSTSEC-2024-0429 is recorded in `gui/shisa-settings/src-tauri/Cargo.lock` as `glib 0.18.5` through `tauri 2.12.5` and `gtk 0.18.2`. The advisory is patched only in `glib >= 0.20.0`, which needs the gtk-rs 0.20 stack. On 2026-11-22 the newest stable `tauri 2.12.7`, `wry 0.57.3`, `tao 0.37.3` and `muda 0.21.2` all still require `gtk ^0.18`, and `tauri 3.0.0-alpha.6` does too. `tao`'s development branch requires `gtk 0.19`, which the advisory does not cover, and the gtk-rs `0.18` branch still passes `&p` to `g_variant_get_child`. `glib 0.18.5` is the last 0.18 release. Shisa does not call `VariantStrIter`. The alert was dismissed as tolerable risk on 2026-11-22. Revisit it when a stable Tauri release moves to gtk-rs 0.20, then upgrade and confirm `cargo tree -i glib` reports `>= 0.20`.

The `atty` change was verified with the full check list and a local test package `shisa-0.1.3-1.1`. `cargo fmt --all -- --check`, `cargo test --workspace` (164 passed), `cargo clippy --workspace --all-targets`, the frontend build, `npm test` (9 Node and 7 vitest tests), the settings Tauri tests (3) and the KWin script tests (12) passed. `makepkg`'s `check()` passed. The package was installed with `pacman -U`, and `pacman -Qkk shisa` reported 0 changed files out of 49. `shisad` and `shisa-agent` both restarted and reported active. The agent logged `shisad is gone` and `registered with shisad` across the daemon restart, and the StatusNotifierWatcher listed `org.kde.StatusNotifierItem-1234760-1/StatusNotifierItem`. The owner verified the tray icon, tooltip, right-click menu, settings window, restore panel and Lock all in the live Wayland session.
