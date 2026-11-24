# Packaging and installation verification

## Packages

| Distribution | Recipe                    | Build environment      |
| ------------ | ------------------------- | ---------------------- |
| Arch Linux   | `packaging/arch/PKGBUILD` | `archlinux:base-devel` |
| Fedora       | `packaging/nfpm.yaml`     | `fedora:latest`        |
| Debian       | `packaging/nfpm.yaml`     | `debian:12`            |

The [packages workflow](../.github/workflows/packages.yml) builds all three on a published release and on manual dispatch. The workflow runs the Rust workspace tests, the settings window tests, the Tauri tests, and the KWin script tests across its jobs: the frontend job runs the settings and KWin script suites, the Fedora and Debian jobs run the Rust workspace and Tauri suites, and the Arch job's `makepkg` check covers all four. The Fedora and Debian jobs build inside their own distribution container and package the four binaries with nfpm. The Arch job keeps the PKGBUILD and the libalpm hook.

The Fedora RPM depends on `polkit`, `dbus`, `webkit2gtk4.1`, `gtk3`, `hicolor-icon-theme`, and `systemd`. The Debian DEB depends on `polkitd`, `dbus`, `libwebkit2gtk-4.1-0`, `libgtk-3-0`, `hicolor-icon-theme`, and `systemd`. Both packages install the system and user units, the polkit action and rule, the D-Bus policy, the sysusers entry, the desktop file, and the icons. Their postinstall creates the `shisa-lock` account, reloads the D-Bus policy, reloads a running `shisad`, and prints the password and service commands. They do not enable a service or set a password.

## Build a local test package

Run these commands from the repository root on Arch Linux:

```sh
build_dir=$(mktemp -d "${TMPDIR:?}/shisa-arch.XXXXXXXX")
packaging/arch/prepare-build.sh --worktree "$build_dir"
(cd "$build_dir" && SRCDEST="$build_dir" makepkg)
```

The source archive includes modified tracked files and non-ignored additions. Deleted files and `packaging/` are excluded. The script stages its own checksum and sets a decimal test `pkgrel` in the copied recipe. The repository's `PKGBUILD` stays unchanged. Install needed build dependencies separately if they are missing. Both the source archive and the generated package stay in the build directory.

A later formal package with the same base `pkgrel` sorts below the local decimal test build. Install the formal package explicitly with `pacman -U` if switching back from the test build.

## Build a Fedora or Debian package locally

Run the build on the distribution you are packaging for. A package built on Arch links Arch's glibc and WebKitGTK, so it is only useful for inspecting the file list, not for installing on Fedora or Debian. The CI builds inside the matching container. Build the four binaries and the settings assets first, then package them with nfpm. The version comes from the PKGBUILD for a worktree build or from the release tag in CI.

```sh
version=$(sed -n 's/^pkgver=//p' packaging/arch/PKGBUILD)
out=$(mktemp -d "${TMPDIR:?}/shisa-nfpm.XXXXXXXX")
mkdir -p "$out/bin"
(cd gui/shisa-settings && npm ci && npm run build)
cargo build --release --locked
(cd gui/shisa-settings/src-tauri && cargo build --release --locked --features custom-protocol)
install -Dm755 target/release/shisad target/release/shisa target/release/shisa-agent "$out/bin/"
install -Dm755 gui/shisa-settings/src-tauri/target/release/shisa-settings "$out/bin/"

docker run --rm \
  --user "$(id -u):$(id -g)" \
  -e HOME=/tmp \
  -e VERSION="$version" \
  -e ARCH=amd64 \
  -e BINDIR=/out/bin \
  -v "$PWD:/src:ro" \
  -v "$out:/out" \
  -w /src \
  goreleaser/nfpm:v2.49.0 package --config packaging/nfpm.yaml --packager deb --target /out
```

Change `--packager deb` to `--packager rpm` for the Fedora package. The local package uses the same recipes as CI.

## Distribution-specific verification

The Fedora and Debian packages are built by CI, but their window layer and Fedora's SELinux policy are not verified on a live KDE session yet. Treat them as untested until the checks below are recorded.

- Fedora KDE: install the RPM, set the password, enable both services, lock a protected window, restore it inside and outside the grace period, and check `ausearch -m avc -ts recent` for SELinux denials from `fanotify_init`, `fanotify_mark`, `/proc/<pid>/exe`, or `/etc/shadow`. If a denial appears, the package needs a policy module or a documented unconfined service label.
- Debian KDE: install the DEB and run the same window checks. AppArmor has no Shisa profile, so no policy change is expected.
- Both: record the KWin version from `kwin_wayland --version` with the result. The window script is tested against KWin 6.8.3 on Arch.
- The Fedora RPM is built on the current `fedora:latest` image and the Debian DEB on `debian:12`. The Fedora package therefore targets that Fedora release and newer. A broader Fedora range needs an older build image.

## GitHub Release packages

The [packages workflow](../.github/workflows/packages.yml) builds the commit selected by a published `vX.Y.Z` Release. The tag must match the tagged `PKGBUILD` version. The Arch job runs `makepkg` and `check()` in a fresh Arch Linux image and checks the installed file list. `check()` runs the Rust workspace tests, the settings window's Node and vitest suites, its Tauri tests, and the KWin script test. The Fedora and Debian jobs build in `fedora:latest` and `debian:12` and package the binaries with nfpm. The upload job attaches the three x86_64 packages and one combined `SHA256SUMS` to that same Release. It preserves existing assets and reports a failure if an existing asset differs from the build. A failed distro build still uploads the packages that succeeded, then fails the job. Delete the partial `SHA256SUMS` from the release before re-running so the full checksum file can replace it. The [Arch package workflow](../.github/workflows/arch-package.yml) remains as a manual Arch-only build.

Use **Actions → Build packages → Run workflow** to build all three packages as Actions artifacts from a selected branch or tag. Use **Actions → Build Arch package → Run workflow** for an Arch-only build. A manual run builds test packages and does not upload Release assets. The workflow assumes release assets can be added after publication.

On 2026-11-21, the v0.1.1 release build failed twice in the container: one test required a systemd user manager to export `WAYLAND_DISPLAY`, and another resolved `/usr/bin/flatpak` from the host. Both tests no longer depend on the host, and the rebuilt release attached `shisa-0.1.1-1-x86_64.pkg.tar.zst` and `SHA256SUMS`.

On 2026-11-21, v0.1.2 built and passed `check()` in the release container. The release attached `shisa-0.1.2-1-x86_64.pkg.tar.zst`, `SHA256SUMS` and `shisa-0.1.2.tar.gz`. The downloaded package and source archive matched their checksums, and the package lists the four binaries, both systemd units and the polkit action and rule.

For manual AUR preparation, `packaging/arch/release.sh` still creates the tagged source archive, updates the hash, and writes `.SRCINFO`. That source archive can be uploaded separately when needed. `make-source.sh` refuses a tag whose workspace `Cargo.toml`, settings `Cargo.toml`, `package.json`, and `tauri.conf.json` do not all name the PKGBUILD version. The settings window compares its own version with shisad's, so a manifest that lags behind would leave a permanent mismatch notice on a correct install.

## Installation and upgrade verification

With approval, install the generated package with `pacman -U` on Arch, `dnf install` for the RPM, or `apt install` for the DEB. After upgrading, restart `shisad.service` and the user unit `shisa-agent.service` through the same approval process, then reopen the settings window. Set a separate app-lock password on first install as shown in the [README](../README.md#installation). Inspect the installed files with `pacman -Qkk shisa`, `rpm -V shisa`, or `dpkg -V shisa`, and follow [Tests and verification](testing.md) for the affected behavior.

On 2026-11-17, upgrading a temporary Arch package replaced a protected file and the post-transaction hook marked the new inode before the 30-second polling interval elapsed. On 2026-11-18, the package built with `extra-x86_64-build` in a clean chroot.

On 2026-11-22, four local test packages `0.1.2-1.1` were built from the worktree and three were installed over `0.1.2-1` with `pacman -U`. Each install left `pacman -Qkk shisa` with no changed files. The first restarted both services and confirmed them active, and the later two replaced only the settings frontend. The settings window reopened from `/usr/bin/shisa-settings` after clearing `$XDG_RUNTIME_DIR/shisa-settings`, and its built assets were checked for the new sidebar icons, the trimmed copy and the `system-ui` font stack. The `inter-font` optdepend was removed at the same time.

On 2026-11-22, v0.1.3 built and passed `check()` in the release container. The release attached `shisa-0.1.3-1-x86_64.pkg.tar.zst`, `SHA256SUMS` and `shisa-0.1.3.tar.gz`. The downloaded package and source archive matched their checksums.

On 2026-11-22, a local test package `shisa-0.1.3-1.1` was built from the worktree with `prepare-build.sh --worktree`. `makepkg`'s `check()` passed the workspace tests, the settings frontend and Tauri tests, and the KWin script tests, and `pacman -Qkp` found every file. It was installed over `0.1.2-1.1` with `pacman -U`. `pacman -Qkk shisa` reported 0 changed files, both services restarted at 11:27:29, and `Diagnostics` reported version 0.1.3. The live Wayland verification of restoring a focus-loss-hidden window inside and outside its grace period is still pending.

On 2026-11-22, a second local test package `shisa-0.1.3-1.2` added the app-tray restore path. `makepkg`'s `check()` passed, including the KWin script tests for a hidden app activation that asks `RestoreIfTicketed` without showing the window first. It was installed over `0.1.3-1.1` with `pacman -U` and `pacman -Qkk shisa` reported 0 changed files. Only `shisa-agent.service` was restarted, so `shisad` kept its PID and the existing tickets and window permissions. The active script in `/run/user/1000/shisa` contains `RestoreIfTicketed`. The user verified the app-tray restore path in the live Wayland session.

On 2026-11-22, a new local test package `shisa-0.1.3-1.1` was built from the worktree with the policy-mode, scan-budget, and test-harness changes. `makepkg`'s `check()` passed the workspace tests, the settings frontend and Tauri tests, and the KWin script tests. `pacman -Qip` reports the install script, and `bsdtar` shows the four binaries, both units, the polkit action and rule, the D-Bus policy, the sysusers entry, the pacman hook, `ConfigurationDirectoryMode=0700` in `shisad.service`, and `secure_config` in `.INSTALL`. The package was not installed.

On 2026-11-22, a local test package `shisa-0.1.3-1.1` was built from the `audit/small-hardening` worktree. `makepkg`'s `check()` passed the workspace tests, the settings frontend and Tauri tests, and the KWin script tests. `pacman -Qip` reports the install script, and the package carries the `secure_config` migration in `.INSTALL` and `ConfigurationDirectoryMode=0700` in `shisad.service`. The package was not installed.

On 2026-11-22, the local test package `shisa-0.1.3-1.1` was installed over the installed 0.1.3-1.1 with `pacman -U`. `pacman -Qkk shisa` reported 0 changed files out of 49. `post_upgrade` tightened `/etc/shisa/users` to 0700 and its files to 0600. `shisad.service` and `shisa-agent.service` were restarted, both active, and `Diagnostics` reported 0.1.3. The user verified the live KWin checks.

On 2026-11-22, the local test package from the `audit/session-env-hardening` worktree was installed over 0.1.3-1.1 with `pacman -U`. `pacman -Qkk shisa` reported 0 changed files out of 49. The config modes stayed 0700/0600. `shisad.service` and `shisa-agent.service` were restarted, both active, and `Diagnostics` reported 0.1.3 with the configuration-failure timestamp at zero. The user verified the live settings checks.

On 2026-11-22, local test packages 0.1.4-1.1 were built from the worktree and installed over 0.1.4-1.1 with `pacman -U`. `makepkg`'s `check()` passed the workspace, settings frontend, Tauri, and KWin script suites. `pacman -Qkk shisa` reported 0 changed files out of 52. `shisad.service` and `shisa-agent.service` were restarted, both active. The user verified the live notification, health-line, transient-scope, and empty-restore-panel flows.

On 2026-11-22, a local test package `shisa-0.1.4-1.1` was built from the worktree with `prepare-build.sh --worktree`. `makepkg`'s `check()` passed the workspace tests, the settings frontend (9 Node and 12 vitest tests), the settings Tauri tests (5), and the KWin script tests (14). `pacman -Qkp` found all 51 files. The package was not installed here. The live checks of the restore panel's two-second refresh, the duplicate-name ids, the folded diagnostics card and the tooltip without the Do Not Disturb advice are still pending.

On 2026-11-22, a second local test package `shisa-0.1.4-1.1` was built from the worktree after the single-instance GTK main-thread fix. `makepkg`'s `check()` passed the same suites, and `pacman -Qkp` found all 51 files. It was installed over 0.1.4-1.1 with `pacman -U`, `pacman -Qkk shisa` reported 0 changed files out of 52, and `shisad.service` and `shisa-agent.service` restarted, both active. The installed binary was then checked in the live session: with the settings window open, a second `shisa-settings --restore-windows` produced the `恢复窗口` frame and left the settings process running, instead of the earlier GTK main-thread panic and disconnected peer. The settings and restore windows from the check were then closed. The tooltip's Do Not Disturb exclusion was verified later with a running Motrix window and an authenticated restore, as recorded in the window-management reference. The duplicate-name ids remain covered by unit tests only.

On 2026-11-23, v0.1.5 built and passed `check()` in the release container. The tag `v0.1.5` points to `4fd1f1c`. The release attached `shisa-0.1.5-1-x86_64.pkg.tar.zst`, `SHA256SUMS` and `shisa-0.1.5.tar.gz`. The downloaded package matched `SHA256SUMS`, the source archive matched the PKGBUILD checksum, the package reports version 0.1.5-1 from `Shisa GitHub Actions`, and `pacman -Qkp` lists all 51 files.

On 2026-11-23, the cross-distribution packaging changes passed the local checks. `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked`, `cargo test --workspace --locked`, `npm test` in `gui/shisa-settings`, and `node --test crates/shisa-agent/tests/*.test.mjs` passed. `bash -n` accepted `packaging/scripts/postinstall.sh`, `packaging/scripts/preremove.sh`, `.github/scripts/build-fedora.sh`, and `.github/scripts/build-debian.sh`. nfpm 2.49.0 then built a DEB and an RPM from placeholder binaries in a bwrap sandbox. The DEB control named the six configured dependencies, `postinst` and `prerm` were present, and the payload carried the four binaries with mode 0755, both units, the polkit action and rule, the D-Bus policy, the sysusers entry, the desktop file, the icons, and the Debian copyright. The embedded `postinst` and `prerm` contained the fresh-install/upgrade branches and the `upgrade`/`deconfigure` removal guard. The RPM payload carried the same files plus `/usr/share/licenses/shisa/LICENSE` and no Debian copyright. `shellcheck` accepted the four scripts and `actionlint` accepted the workflows after the partial-upload change. The real binaries, the Fedora container build, package installation, and the Fedora and Debian live KDE checks remain pending. The first packages workflow run is the first build with the real binaries.

On 2026-11-23, a local test package `shisa-0.1.5-1.1` was built from the worktree with `prepare-build.sh --worktree`. `makepkg`'s `check()` passed the workspace tests, the settings frontend (9 Node and 12 vitest tests), the settings Tauri tests (5), and the KWin script tests (14). It was installed over `0.1.5-1` with `pacman -U`, `pacman -Qkk shisa` reported 0 changed files out of 52, and `shisad.service` and `shisa-agent.service` restarted, both active. The user reported the verification steps passing in the live Plasma Wayland session, and the README screenshots were regenerated from the installed build.

On 2026-11-23, a local test package `shisa-0.1.5-1.1` with the four eye-color tray icons was built from the worktree. `makepkg`'s `check()` passed, it installed over the previous `0.1.5-1.1`, and `pacman -Qkk shisa` reported 0 changed files out of 55. Only `shisa-agent.service` was restarted, so shisad kept the existing tickets and window permissions. Plasma served the old icon pixmaps for the existing icon names until `plasma-plasmashell.service` was restarted. The user confirmed the tray after that restart.

On 2026-11-24, v0.1.6 built in the packages workflow (run 45212387333) and all five jobs passed. The tag `v0.1.6` points to `128f2c0`. The release attached `shisa-0.1.6-1-x86_64.pkg.tar.zst`, `shisa-0.1.6-1.x86_64.rpm`, `shisa_0.1.6-1_amd64.deb`, `SHA256SUMS` and `shisa-0.1.6.tar.gz`. The three packages matched `SHA256SUMS`, the Arch and Debian metadata reports version 0.1.6-1, and the RPM name and embedded NEVRA are `shisa-0.1.6-1`. The three payloads carry the four tray icons at 16x16, 22x22 and scalable sizes, including `shisa-tray-restorable-symbolic.svg`. The source archive's sha256 `3c51256b27f9f9f7512c69d18faf3b0d484afd61a7b1e4e0f1ce3bab761f5c02` matches the PKGBUILD. The Fedora and Debian live KDE checks are still pending.

## Dependency advisories

On 2026-11-22, the two open Dependabot alerts were reviewed.

- `atty` GHSA-g98v-hv3f-hcfr / RUSTSEC-2021-0145 entered only as a build dependency of `ksni 0.2.2`: `dbus-codegen 0.9.1` and `clap 2.34.0` pulled in `atty 0.2.14`. The agent now uses `ksni 0.3.7`, which speaks zbus directly. `cargo tree -i atty` reports that no package matches, and `Cargo.lock` contains neither `atty` nor `dbus-codegen` nor `clap 2`. The advisory is Windows-only and build-time, so the agent binary's runtime behavior is unchanged.
- `glib` GHSA-wrw7-89jp-8q8g / RUSTSEC-2024-0429 is recorded in `gui/shisa-settings/src-tauri/Cargo.lock` as `glib 0.18.5` through `tauri 2.12.5` and `gtk 0.18.2`. The advisory is patched only in `glib >= 0.20.0`, which needs the gtk-rs 0.20 stack. On 2026-11-22 the newest stable `tauri 2.12.7`, `wry 0.57.3`, `tao 0.37.3` and `muda 0.21.2` all still require `gtk ^0.18`, and `tauri 3.0.0-alpha.6` does too. `tao`'s development branch requires `gtk 0.19`, which the advisory does not cover, and the gtk-rs `0.18` branch still passes `&p` to `g_variant_get_child`. `glib 0.18.5` is the last 0.18 release. Shisa does not call `VariantStrIter`. The alert was dismissed as tolerable risk on 2026-11-22. Revisit it when a stable Tauri release moves to gtk-rs 0.20, then upgrade and confirm `cargo tree -i glib` reports `>= 0.20`.

The `atty` change was verified with the full check list and a local test package `shisa-0.1.3-1.1`. `cargo fmt --all -- --check`, `cargo test --workspace` (164 passed), `cargo clippy --workspace --all-targets`, the frontend build, `npm test` (9 Node and 7 vitest tests), the settings Tauri tests (3) and the KWin script tests (12) passed. `makepkg`'s `check()` passed. The package was installed with `pacman -U`, and `pacman -Qkk shisa` reported 0 changed files out of 49. `shisad` and `shisa-agent` both restarted and reported active. The agent logged `shisad is gone` and `registered with shisad` across the daemon restart, and the StatusNotifierWatcher listed `org.kde.StatusNotifierItem-1234760-1/StatusNotifierItem`. The owner verified the tray icon, tooltip, right-click menu, settings window, restore panel and Lock all in the live Wayland session.
