<p align="center">
  <img src="data/icons/shisa.svg" width="96" height="96" alt="Shisa icon">
</p>

# Shisa

[![Test](https://github.com/RiverOnVenus/Shisa/actions/workflows/test.yml/badge.svg)](https://github.com/RiverOnVenus/Shisa/actions/workflows/test.yml)

English · [简体中文](README.zh-CN.md)

Shisa is an app lock for Linux with KDE Plasma 6 on Wayland. Its main use is briefly lending your unlocked desktop: hide private windows such as chat and mail, then require a separate app-lock password to restore them or launch their apps again through a protected entry point.

> **Note:** Shisa is not a screen lock and not a user isolation tool. It offers only limited protection. It is meant for briefly lending an unlocked desktop to a child, family member, or friend, or for keeping private windows out of sight for a while. A determined attacker, or anyone who knows the login password, can bypass it. See [Protection scope](#protection-scope) for the specific limits.

![Shisa Settings](docs/images/settings.png)

## Installation

Download the package for your distribution from [GitHub Releases](https://github.com/RiverOnVenus/Shisa/releases), install `shisa` from the AUR on Arch Linux, or [build a local test package](docs/packaging.md#build-a-local-test-package), then complete the initial setup:

```sh
# Arch Linux (AUR)
paru -S shisa
# Arch Linux (release package)
sudo pacman -U ./shisa-<version>-1-x86_64.pkg.tar.zst
# Fedora
sudo dnf install ./shisa-<version>-1.x86_64.rpm
# Debian or Ubuntu
sudo apt install ./shisa_<version>-1_amd64.deb

sudo passwd shisa-lock
sudo systemctl enable --now shisad.service
systemctl --user daemon-reload
systemctl --user enable --now shisa-agent.service
```

The app-lock password is separate from your login password. After upgrading, restart both services and reopen the settings window. The Fedora and Debian packages are built in CI and have no live KDE verification yet.

## Usage

1. Open **Shisa Settings** and enable **Require authentication before launch** for the apps you want to protect.
2. Before lending your desktop, right-click the Shisa tray icon and choose **Lock protected apps**. Check the notification to confirm the protected windows were hidden. It also reminds you to enable Do Not Disturb when that is off.
3. When you get your desktop back, left-click the tray icon, or choose **Restore windows** from its right-click menu. Select an app and authenticate. The original window state and unsaved content are preserved.

Bind `shisa lock` to a KDE keyboard shortcut for the same manual lock from the keyboard. **Hide when focus is lost** is a separate convenience: it hides a window after it loses focus but keeps the app's launch ticket, so restoring it within the grace period does not ask for the app-lock password. It does not replace **Lock protected apps** before lending the desktop.

The Shisa tray icon is a keyhole inside a ring, and the ring carries the state:

|                                   Icon                                    | Color | Meaning                                                                         |
| :-----------------------------------------------------------------------: | ----- | ------------------------------------------------------------------------------- |
|   <img src="docs/images/tray-ready.svg" width="24" height="24" alt="">    | Green | No hidden windows and no app in its grace period                                |
|   <img src="docs/images/tray-locked.svg" width="24" height="24" alt="">   | Blue  | A hidden window needs the app-lock password to restore                          |
| <img src="docs/images/tray-restorable.svg" width="24" height="24" alt=""> | Amber | A hidden app can restore, or another app can still launch, without the password |
|  <img src="docs/images/tray-offline.svg" width="24" height="24" alt="">   | Red   | shisad is offline                                                               |

Closing a protected app's last window drops its window permission. A reopen inside the grace period still asks no password, and a later one goes through Shisa's restore panel. The tooltip says how many hidden apps are password-free and how many apps can still launch without the password in their grace period.

## Protection scope

Launch authentication covers only protected entry points that are active. Apps running under the same account can still read application data, which is not encrypted, or interfere with window hiding. Hiding covers windows only, so notifications from hidden apps still appear and the notification history can still be opened.

See the [threat model](docs/architecture.md#threat-model) for the situations Shisa covers and the ones it does not.

## Development

Run `cargo test --workspace` to check the Rust workspace. To build the settings app, run `npm ci` and `npm run build` in `gui/shisa-settings`, then run `cargo build --release --features custom-protocol` in `gui/shisa-settings/src-tauri`.

See the [developer documentation](docs/README.md) for architecture, coding standards, and testing, and the [packaging guide](docs/packaging.md) for distribution builds and upgrade verification.

## License

Copyright (C) 2026 RiverOnVenus.

[GPL-3.0-or-later](LICENSE)
