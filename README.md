# Shisa

English · [简体中文](README.zh-CN.md)

Shisa is a desktop app lock for Arch Linux and KDE Plasma 6 on Wayland. When lending someone your unlocked desktop, you can hide private windows and require a separate password to restore them or launch apps through protected entry points.

## Installation

Download the x86_64 Arch package from [GitHub Releases](https://github.com/RiverOnVenus/Shisa/releases) when available, or [build a local test package](docs/packaging.md#build-a-local-test-package). Install the package and complete the initial setup:

```sh
sudo pacman -U ./shisa-0.1.1-1-x86_64.pkg.tar.zst
sudo passwd shisa-lock
sudo systemctl enable --now shisad.service
systemctl --user enable --now shisa-agent.service
```

Replace the example package filename with the file you downloaded or built. The app-lock password is separate from your login password. After upgrading, restart both services and reopen the settings window.

## Usage

1. Open **Shisa Settings** and enable **Require authentication before launch** for the apps you want to protect.
2. Before lending your desktop, right-click the Shisa tray icon and choose **Lock all**. Check the notification to confirm that the protected windows were hidden.
3. When you get your desktop back, left-click the tray icon, select an app in **Restore windows**, and authenticate. The original window and unsaved content are preserved.

You can bind `shisa lock` to a KDE keyboard shortcut or enable **Hide when focus is lost** per app.

The interface follows your desktop locale. To use the other language, open **General** and choose **简体中文** or **English** under **Language**. The tray, notifications and `shisa` commands follow the same choice.

## Protection scope

Shisa guards against casually opening private apps. Launch authentication applies to active protected paths. Apps running under the same account can still read application data or interfere with window hiding. Use separate system accounts when you need to isolate app data and files.

## Development

Run `cargo test --workspace` to check the Rust workspace. To build the settings app, run `npm ci` and `npm run build` in `gui/shisa-settings`, then run `cargo build --release --features custom-protocol` in `gui/shisa-settings/src-tauri`.

See the [developer documentation](docs/README.md) for architecture, coding standards, and testing, and the [packaging guide](docs/packaging.md) for Arch builds and upgrade verification.

## License

[GPL-3.0-or-later](LICENSE)
