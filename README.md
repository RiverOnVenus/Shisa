# Shisa

English · [简体中文](README.zh-CN.md)

Shisa is a desktop app lock for Arch Linux and KDE Plasma 6 on Wayland. When lending someone your unlocked desktop, you can hide private windows and require a separate password to restore them or launch apps through protected entry points.

## Installation

Shisa currently uses a locally built Arch package. Once you have the package, install it and complete the initial setup:

```sh
sudo pacman -U ./shisa-0.1.0-1-x86_64.pkg.tar.zst
sudo passwd shisa-lock
sudo systemctl enable --now shisad.service
systemctl --user enable --now shisa-agent.service
```

Replace the example package filename with the file you built. The app-lock password is separate from your login password. After upgrading, restart both services and reopen the settings window.

## Usage

1. Open **Shisa Settings** and enable **Require authentication before launch** (启动前要求认证) for the apps you want to protect.
2. Before lending your desktop, right-click the Shisa tray icon and choose **Lock all** (全部上锁). Check the notification to confirm that the protected windows were hidden.
3. When you get your desktop back, left-click the tray icon, select an app in **Restore windows** (恢复窗口), and authenticate. The original window and unsaved content are preserved.

You can bind `shisa lock` to a KDE keyboard shortcut or enable **Hide on focus loss** (失焦后隐藏) per app.

## Protection scope

Shisa guards against casually opening private apps. Launch authentication applies to active protected paths. Apps running under the same account can still read application data or interfere with window hiding. Use separate system accounts when you need to isolate app data and files.

## Development

Run `cargo test --workspace` to check the Rust workspace. To build the settings app, run `npm ci` and `npm run build` in `gui/shisa-settings`, then run `cargo build --release --features custom-protocol` in `gui/shisa-settings/src-tauri`.

See the [developer documentation](docs/README.md) for architecture, coding standards, and testing, and the [packaging guide](docs/packaging.md) for Arch builds and upgrade verification.

## License

[GPL-3.0-or-later](LICENSE)
