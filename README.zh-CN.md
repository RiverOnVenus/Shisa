# Shisa

[English](README.md) · 简体中文

Shisa 是 Arch Linux 和 KDE Plasma 6（Wayland）上的桌面应用锁。临时把电脑借给别人时，可以收起聊天、邮件等窗口，并要求输入应用锁密码才能恢复窗口或从受保护的入口启动应用。

## 安装

在 [GitHub Releases](https://github.com/RiverOnVenus/Shisa/releases) 下载可用的 x86_64 Arch 包，或[在本地构建测试包](docs/packaging.md#build-a-local-test-package)。然后安装并完成首次设置：

```sh
sudo pacman -U ./shisa-0.1.2-1-x86_64.pkg.tar.zst
sudo passwd shisa-lock
sudo systemctl enable --now shisad.service
systemctl --user enable --now shisa-agent.service
```

请把第一行的包文件名换成实际文件名。应用锁密码与登录密码分开。升级后重启这两个服务，再重新打开设置窗口。

## 使用

1. 打开 **Shisa 设置**，给需要保护的应用开启「启动前要求认证」。
2. 借出电脑前，右键点击 Shisa 托盘图标，选择「全部上锁」，查看通知确认窗口已收起。
3. 拿回电脑后，左键点击托盘图标，在「恢复窗口」面板选择应用并认证。原窗口和未保存的内容会保留。

可以在 KDE 的快捷键设置中绑定 `shisa lock`，也可以按应用设置「失焦后隐藏」。

界面语言默认跟随系统 locale。要换用另一种语言，打开「通用」，在「语言」中选择「简体中文」或「English」。托盘、通知和 `shisa` 命令会跟着使用同一种语言。

## 保护范围

Shisa 适合防止别人顺手打开应用。启动认证仅覆盖实际生效的保护路径，应用数据不会加密，同一账号下的程序仍可能绕过窗口隐藏。需要隔离文件和应用数据时，请使用独立的系统账号。

## 开发

运行 `cargo test --workspace` 检查 Rust 工作区。构建设置程序时，先在 `gui/shisa-settings` 运行 `npm ci` 和 `npm run build`，再在 `gui/shisa-settings/src-tauri` 运行 `cargo build --release --features custom-protocol`。

架构、开发规范和测试入口见[开发文档](docs/README.md)。Arch 构建与升级验证见[打包指南](docs/packaging.md)。

## 许可证

[GPL-3.0-or-later](LICENSE)
