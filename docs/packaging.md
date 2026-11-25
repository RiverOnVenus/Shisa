# Packaging and installation verification

## Packages

| Distribution | Recipe                    | Build environment      |
| ------------ | ------------------------- | ---------------------- |
| Arch Linux   | `packaging/arch/PKGBUILD` | `archlinux:base-devel` |
| Fedora       | `packaging/nfpm.yaml`     | `fedora:latest`        |
| Debian       | `packaging/nfpm.yaml`     | `debian:12`            |

The [packages workflow](../.github/workflows/packages.yml) builds all three on a published release and on manual dispatch. The tag must match the tagged `PKGBUILD` version. The frontend job runs the settings and KWin script suites. The Arch job runs `makepkg` and `check()` in a fresh Arch Linux image and checks the installed file list. `check()` runs the Rust workspace tests, the settings window's Node and vitest suites, its Tauri tests, and the KWin script test. The Fedora and Debian jobs build in `fedora:latest` and `debian:12`, run the Rust workspace and Tauri suites, and package the four binaries with nfpm. The Arch job keeps the PKGBUILD and the libalpm hook.

The upload job attaches the three x86_64 packages and one combined `SHA256SUMS` to the release. It preserves existing assets, compares them byte for byte, and fails if an existing asset differs from the build. A failed distro build still uploads the packages that succeeded, then fails the job. Delete the partial `SHA256SUMS` from the release before re-running so the full checksum file can replace it.

A manual run builds test packages and uploads Actions artifacts, not release assets. Use **Actions → Build packages → Run workflow** for all three, or **Actions → Build Arch package → Run workflow** from the [Arch package workflow](../.github/workflows/arch-package.yml) for Arch only. The workflow assumes release assets can be added after publication.

A release build must not depend on the host. The v0.1.1 build failed twice on a test that needed a systemd user manager to export `WAYLAND_DISPLAY` and on one that resolved `/usr/bin/flatpak` from the host. Both tests now skip in containers or resolve their own paths.

The Fedora RPM depends on `polkit`, `dbus`, `webkit2gtk4.1`, `gtk3`, `hicolor-icon-theme`, and `systemd`. The Debian DEB depends on `polkitd`, `dbus`, `libwebkit2gtk-4.1-0`, `libgtk-3-0`, `hicolor-icon-theme`, and `systemd`. Both packages install the system and user units, the polkit action and rule, the D-Bus policy, the sysusers entry, the desktop file, and the icons. Their postinstall creates the `shisa-lock` account, reloads the D-Bus policy, reloads a running `shisad`, and prints the password and service commands. If an active D-Bus cannot be reloaded, the postinstall prints a warning and asks for a reboot instead of discarding the failure. They do not enable a service or set a password.

## Release procedure

The source archive and its checksum depend on the tag, so keep this order.

1. Bump the workspace, settings, and Tauri versions, open a pull request titled `release: X.Y.Z`, and merge it.
2. Tag the merged commit `vX.Y.Z` and push the tag.
3. Run `packaging/arch/release.sh` from `packaging/arch/`. It builds `shisa-X.Y.Z.tar.gz` from the tag, writes its sha256 into `PKGBUILD`, writes `.SRCINFO`, and prints the upload URL. Upload that exact tarball to the URL before `makepkg` can download it.
4. Commit the checksum. Once the packages workflow finishes, record the result in [Verification state](#verification-state). Title the pull requests `packaging: X.Y.Z checksum and release record` or `docs: record the X.Y.Z release`.
5. Publish the GitHub release for the tag with generated notes. The [release notes workflow](../.github/workflows/release-notes-label.yml) adds `skip-changelog` to release and post-release housekeeping pull requests, and [.github/release.yml](../.github/release.yml) keeps that label out of the generated notes. Check that only feature pull requests appear.
6. Copy `PKGBUILD`, `shisa.install`, and `.SRCINFO` to the AUR checkout and build once in a clean chroot with `extra-x86_64-build` or `paru -S --chroot aur/shisa`.

`make-source.sh` refuses a tag whose workspace `Cargo.toml`, settings `Cargo.toml`, `package.json`, and `tauri.conf.json` do not all name the PKGBUILD version. The settings window compares its own version with `shisad`'s, so a manifest that lags behind would leave a permanent mismatch notice on a correct install.

The source archive excludes `packaging/`, so the checksum commit does not change the tarball or the checksum.

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

## Verification state

The table records the latest actual run, not what the recipes should do. Per-change run details live in the [verification log](testing.md#verification-log).

| Area                                 | State                                                                                                                             | Evidence                                                                              |
| ------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------- |
| Arch build and `check()`             | Verified through 0.1.7                                                                                                            | Packages workflow run 45527680167, all five jobs passed                               |
| Arch install and upgrade             | Verified through 0.1.7                                                                                                            | `pacman -U` followed by `pacman -Qkk` reporting 0 changed files, both services active |
| Arch live window, tray, and settings | Verified on 0.1.7 for the tray ticket-state transition, and on earlier releases for the window, restore panel, and settings flows | Manual Plasma Wayland checks                                                          |
| Fedora RPM                           | Built and payload-checked through 0.1.7. Live KDE and SELinux checks pending.                                                     | Packages workflow                                                                     |
| Debian DEB                           | Built and payload-checked through 0.1.7. Live KDE and AppArmor checks pending.                                                    | Packages workflow                                                                     |
| Source archive checksum              | Matches the PKGBUILD for 0.1.2, 0.1.3, 0.1.5, 0.1.6, and 0.1.7                                                                    | `SHA256SUMS` and `makepkg`                                                            |
| Clean chroot build                   | Verified on 2026-11-18                                                                                                            | `extra-x86_64-build`                                                                  |

The 0.1.7 release attached `shisa-0.1.7-1-x86_64.pkg.tar.zst`, `shisa-0.1.7-1.x86_64.rpm`, `shisa_0.1.7-1_amd64.deb`, `SHA256SUMS`, and `shisa-0.1.7.tar.gz`. All three packages matched `SHA256SUMS`, and the source archive's sha256 matched the PKGBUILD. The Fedora and Debian live KDE checks are the main open package checks. The postinstall D-Bus reload warning has only been exercised with a fake `systemctl`. The [verification log](testing.md#verification-log) has the tag commit, earlier release runs, and per-build results.

## Distribution-specific verification

The Fedora and Debian packages are built by CI, but their window layer and Fedora's SELinux policy are not verified on a live KDE session yet. Treat them as untested until the checks below are recorded.

- Fedora KDE: install the RPM, set the password, enable both services, lock a protected window, restore it inside and outside the grace period, and check `ausearch -m avc -ts recent` for SELinux denials from `fanotify_init`, `fanotify_mark`, `/proc/<pid>/exe`, or `/etc/shadow`. If a denial appears, the package needs a policy module or a documented unconfined service label.
- Debian KDE: install the DEB and run the same window checks. AppArmor has no Shisa profile, so no policy change is expected.
- Both: record the KWin version from `kwin_wayland --version` with the result. The window script is tested against KWin 6.8.3 on Arch.
- The Fedora RPM is built on the current `fedora:latest` image and the Debian DEB on `debian:12`. The Fedora package therefore targets that Fedora release and newer. A broader Fedora range needs an older build image.

As of 0.1.7, none of these checks has been recorded. The postinstall D-Bus reload warning was exercised with a fake `systemctl`, but no real package was installed on either distribution.

## Installation and upgrade verification

With approval, install with `pacman -U` on Arch, `dnf install` for the RPM, or `apt install` for the DEB. Set a separate app-lock password on first install as shown in the [README](../README.md#installation). The packages do not restart a running `shisad`, `shisa-agent`, or settings window, so after an upgrade:

1. With approval, restart `shisad.service` and the user unit `shisa-agent.service`.
2. Close the settings window, clear `$XDG_RUNTIME_DIR/shisa-settings`, and reopen it from `/usr/bin/shisa-settings`.
3. Restart `plasma-plasmashell.service` after a tray icon change so Plasma drops its cached pixmaps. The agent does not need a restart unless its binary changed.
4. Run `pacman -Qkk shisa`, `rpm -V shisa`, or `dpkg -V shisa`. On Arch, expect 0 changed files.
5. Confirm `/etc/shisa/users` is 0700 and the files under it are 0600. The 0.1.3 install script migrated these modes.
6. Check that `Diagnostics` reports the package version and that both services are active.

The libalpm hook marks a replaced protected file's new inode during the transaction, before the daemon's 30-second poll. A frontend-only package does not need a service restart, because the settings assets are compiled into the Tauri binary. Follow [Tests and verification](testing.md) for the behavioral checks after an upgrade.

`/etc/shisa` and `/etc/shisa/users` are created by `shisad.service` at startup, not by the install script. Uninstall keeps both, plus the `shisa-lock` account. Remove them by hand if needed. The old `/etc/pam.d/shisa` is no longer part of the package. A modified copy is left as `.pacsave` and `post_upgrade` only warns about it.

## Dependency advisories

On 2026-11-22, the two open Dependabot alerts were reviewed.

- `atty` GHSA-g98v-hv3f-hcfr / RUSTSEC-2021-0145 entered only as a build dependency of `ksni 0.2.2`: `dbus-codegen 0.9.1` and `clap 2.34.0` pulled in `atty 0.2.14`. The agent now uses `ksni 0.3.7`, which speaks zbus directly. `cargo tree -i atty` reports that no package matches, and `Cargo.lock` contains neither `atty` nor `dbus-codegen` nor `clap 2`. The advisory is Windows-only and build-time, so the agent binary's runtime behavior is unchanged. The change was verified with the full check list and a local `0.1.3-1.1` package that installed with `pacman -Qkk` reporting 0 changed files out of 49, and with live tray, menu, settings, and restore checks.
- `glib` GHSA-wrw7-89jp-8q8g / RUSTSEC-2024-0429 is recorded in `gui/shisa-settings/src-tauri/Cargo.lock` as `glib 0.18.5` through `tauri 2.12.5` and `gtk 0.18.2`. The advisory is patched only in `glib >= 0.20.0`, which needs the gtk-rs 0.20 stack. On 2026-11-22 the newest stable `tauri 2.12.7`, `wry 0.57.3`, `tao 0.37.3` and `muda 0.21.2` all still require `gtk ^0.18`, and `tauri 3.0.0-alpha.6` does too. `tao`'s development branch requires `gtk 0.19`, which the advisory does not cover, and the gtk-rs `0.18` branch still passes `&p` to `g_variant_get_child`. `glib 0.18.5` is the last 0.18 release. Shisa does not call `VariantStrIter`. The alert was dismissed as tolerable risk on 2026-11-22. Revisit it when a stable Tauri release moves to gtk-rs 0.20, then upgrade and confirm `cargo tree -i glib` reports `>= 0.20`.
