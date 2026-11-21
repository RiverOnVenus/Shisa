# Shisa

[![Test](https://github.com/RiverOnVenus/Shisa/actions/workflows/test.yml/badge.svg)](https://github.com/RiverOnVenus/Shisa/actions/workflows/test.yml)

[English](README.md) · 简体中文

Shisa 是 Arch Linux 和 KDE Plasma 6（Wayland）上的桌面应用锁。它能防止别人顺手打开你的隐私应用。主要用途是短时间把已解锁的桌面借给别人：收起聊天、邮件等私密窗口，并要求输入单独的应用锁密码才能恢复窗口或从受保护的入口启动应用。

Shisa 不是屏幕锁，也不是用户隔离工具。

![Shisa 设置](docs/images/settings-zh.png)

## 安装

在 [GitHub Releases](https://github.com/RiverOnVenus/Shisa/releases) 下载可用的 x86_64 Arch 包，或[在本地构建测试包](docs/packaging.md#build-a-local-test-package)。然后安装并完成首次设置：

```sh
sudo pacman -U ./shisa-0.1.3-1-x86_64.pkg.tar.zst
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

## 保护范围

Shisa 适合防止别人顺手打开应用。启动认证仅覆盖实际生效的保护路径，应用数据不会加密，同一账号下的程序仍可能绕过窗口隐藏。隐藏只覆盖窗口，被隐藏的应用仍会弹出通知，通知历史也仍然可以打开。借出桌面之前建议开启勿扰模式。

各种情况下的防护边界见[威胁模型](docs/architecture.md#threat-model)。

## 开发

运行 `cargo test --workspace` 检查 Rust 工作区。构建设置程序时，先在 `gui/shisa-settings` 运行 `npm ci` 和 `npm run build`，再在 `gui/shisa-settings/src-tauri` 运行 `cargo build --release --features custom-protocol`。

架构、开发规范和测试入口见[开发文档](docs/README.md)。Arch 构建与升级验证见[打包指南](docs/packaging.md)。

## 许可证

Copyright (C) 2026 RiverOnVenus。

[GPL-3.0-or-later](LICENSE)
