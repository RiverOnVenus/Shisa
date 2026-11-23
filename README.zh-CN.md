# Shisa

[![Test](https://github.com/RiverOnVenus/Shisa/actions/workflows/test.yml/badge.svg)](https://github.com/RiverOnVenus/Shisa/actions/workflows/test.yml)

[English](README.md) · 简体中文

Shisa 是 Linux 上 KDE Plasma 6（Wayland）的应用锁，为 Arch Linux、Fedora 和 Debian 构建安装包。主要用于短时间把已解锁的桌面借给别人：收起聊天、邮件等私密窗口，之后要输入单独的应用锁密码，才能恢复窗口或从受保护的入口重新启动应用。

Shisa 不是屏幕锁，也不是用户隔离工具。

![Shisa 设置](docs/images/settings-zh.png)

## 安装

从 [GitHub Releases](https://github.com/RiverOnVenus/Shisa/releases) 下载对应发行版的包，或在本地[构建测试包](docs/packaging.md#build-a-local-test-package)，然后安装并完成首次设置：

```sh
# Arch Linux
sudo pacman -U ./shisa-<version>-1-x86_64.pkg.tar.zst
# Fedora
sudo dnf install ./shisa-<version>-1.x86_64.rpm
# Debian 或 Ubuntu
sudo apt install ./shisa_<version>-1_amd64.deb

sudo passwd shisa-lock
sudo systemctl enable --now shisad.service
systemctl --user daemon-reload
systemctl --user enable --now shisa-agent.service
```

把 `<version>` 换成你下载的版本。应用锁密码和登录密码是分开的。升级后重启这两个服务，再重新打开设置窗口。Fedora 和 Debian 包由 CI 构建，真机 KDE 验收记录写进[打包指南](docs/packaging.md#distribution-specific-verification)之前按未验证处理。

## 使用

1. 打开 **Shisa 设置**，给需要保护的应用开启「启动前要求认证」。
2. 借出电脑前，右键点击 Shisa 托盘图标，选择「全部上锁」，再查看通知，确认受保护的窗口已收起。勿扰没开时，通知还会提醒你先开启。
3. 拿回电脑后，左键点击托盘图标，在「恢复窗口」面板选择应用并认证。原窗口和其中未保存的内容都会保留。

可以在 KDE 快捷键设置中绑定 `shisa lock`，也可以为单个应用开启「失焦后隐藏」。

## 保护范围

Shisa 防的只是顺手打开隐私应用。启动认证只覆盖当前生效的保护入口，应用数据不加密，同一账号下的程序仍可读取数据，也可能干扰窗口隐藏。隐藏只管窗口，被隐藏的应用照常弹通知，通知历史也照常能打开。上锁通知会在勿扰没开时提醒，借出桌面之前请先开启勿扰模式。

各种情况下的防护边界见[威胁模型](docs/architecture.md#threat-model)。

## 开发

运行 `cargo test --workspace` 检查 Rust 工作区。构建设置程序时，先在 `gui/shisa-settings` 运行 `npm ci` 和 `npm run build`，再在 `gui/shisa-settings/src-tauri` 运行 `cargo build --release --features custom-protocol`。

架构、开发规范和测试方法见[开发文档](docs/README.md)。各发行版的构建与升级验证见[打包指南](docs/packaging.md)。

## 许可证

Copyright (C) 2026 RiverOnVenus。

[GPL-3.0-or-later](LICENSE)
