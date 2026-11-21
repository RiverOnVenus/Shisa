# Shisa

Shisa 是 Arch Linux + KDE Plasma 6 (Wayland) 下的桌面应用锁。把电脑临时递给别人改个文档、选几张照片时，你可以让对方继续用这个桌面，同时收起已打开的聊天、邮件等受保护窗口。恢复这些窗口、或重新启动受保护的程序，都要应用锁密码。

版本 0.1.0，尚未发布到 AUR。

Shisa 读作 SHEE-sah，取自冲绳的シーサー石狮，守门的石狮子。对应到软件，是在应用启动前守一关。

[安装](#安装) · [使用](#使用) · [原理](#原理) · [限制](#限制) · [开发](#开发)

## 能挡住什么

| 场景                                                     | 结论                                       |
| -------------------------------------------------------- | ------------------------------------------ |
| 图标、菜单、KRunner、终端、文件关联、D-Bus 激活、自启动   | 执行已标记的保护路径时拦截                 |
| 停掉 shisad 或改配置                                     | 能防，需要 root                            |
| 在设置里取消保护                                         | 能防，每项改动单独认证                     |
| 知道登录密码但没有 root                                  | 通过受保护入口启动应用仍需应用锁密码       |
| 知道应用锁密码                                           | 不能防                                     |
| 知道登录密码并能取得 root                                | 不能防                                     |
| sudo 或 polkit 凭据缓存未过期                            | 不能防，本机 sudo 时间戳全局有效，30 分钟   |
| 复制二进制到别处、直接调用共享运行时、用解释器读包装脚本 | 不能防                                     |
| 读取应用数据文件                                         | 不能防                                     |
| 已经打开的窗口                                           | 手动上锁时隐藏，也可按锁屏和失焦设置触发    |

窗口层以用户身份运行，同账号的程序可以绕过它，也能伪造一个相似的密码框。shisad 崩溃时保护失效，systemd 会重启它。polkitd 崩溃时认证不可用，不会误放行。短时间大量启动受保护应用可能让 fanotify 队列溢出，那一次 exec 会被放行。不防 root，也不防离线读盘。

这个机制针对的是「别人顺手点开应用」。离席且无人需要电脑时锁屏即可。长期共用电脑、需要保护文件内容，应给使用者开独立的系统账号。

## 安装

仓库里没有打包目录，从源码编译安装：

```sh
git clone RiverOnVenus/Shisa.git
cd Shisa
(cd gui/shisa-settings && npm ci && npm run build)
(cd gui/shisa-settings/src-tauri && cargo build --release --features custom-protocol)
cargo build --release
```

需要 `cargo`、nodejs、npm。设置窗口的前端不带预编译产物，要先构建。

把二进制和 `data/` 里的文件按 FHS 装进系统：

```sh
for bin in shisad shisa shisa-agent; do
    sudo install -Dm755 "target/release/$bin" "/usr/bin/$bin"
done
sudo install -Dm755 gui/shisa-settings/src-tauri/target/release/shisa-settings /usr/bin/shisa-settings

sudo install -Dm644 data/shisad.service /usr/lib/systemd/system/shisad.service
sudo install -Dm644 data/shisa-agent.service /usr/lib/systemd/user/shisa-agent.service
sudo install -Dm644 data/io.github.RiverOnVenus.Shisa.policy /usr/share/polkit-1/actions/
sudo install -Dm644 data/49-shisa-admin.rules /usr/share/polkit-1/rules.d/
sudo install -Dm644 data/shisa-lock.sysusers /usr/lib/sysusers.d/shisa-lock.conf
sudo install -Dm644 data/io.github.RiverOnVenus.Shisa.conf /usr/share/dbus-1/system.d/
sudo install -Dm644 data/shisa-remark.hook /usr/share/libalpm/hooks/90-shisa-remark.hook
sudo install -Dm644 data/io.github.RiverOnVenus.Shisa.desktop /usr/share/applications/

sudo install -Dm644 data/icons/shisa.svg \
    /usr/share/icons/hicolor/scalable/apps/io.github.RiverOnVenus.Shisa.svg
sudo install -Dm644 data/icons/shisa.svg \
    /usr/share/icons/hicolor/scalable/apps/io.github.RiverOnVenus.Shisa-launcher.svg
for size in 16x16 22x22; do
    for icon in data/icons/"$size"/*.svg; do
        sudo install -Dm644 "$icon" "/usr/share/icons/hicolor/$size/status/$(basename "$icon")"
    done
done
for icon in data/icons/22x22/*.svg; do
    sudo install -Dm644 "$icon" "/usr/share/icons/hicolor/scalable/status/$(basename "$icon")"
done
sudo systemd-sysusers
```

手动装的文件不归 pacman 管，卸载要自己删。

设置应用锁密码，它和登录密码分开：

```sh
sudo passwd shisa-lock
```

设好之前受保护应用无法通过认证。`shisa-lock` 不能登录，也没有 sudo 权限，只给 Shisa 的 polkit 对话框用。

启用服务：

```sh
sudo systemctl enable --now shisad.service
systemctl --user enable --now shisa-agent.service
```

重新编译后不会重启正在运行的服务，要手动重启 `shisad` 和 `shisa-agent`，并重开设置窗口。`shisa-lock` 账号不会自动删除，不用了用 `sudo userdel shisa-lock` 清理。

需要 Arch Linux、systemd、KDE Plasma 6、polkit 认证代理（KDE 自带的 `polkit-kde-authentication-agent-1`），以及带 exec 许可事件的 fanotify（Linux 5.0+）。GNOME 和 X11 不在 v1 范围。

## 使用

借出去之前：在设置里勾选要保护的应用，从托盘点「全部上锁」，看一眼通知里的匹配结果。拿回来后在托盘点那个应用，输入应用锁密码。上锁只隐藏窗口，进程和正在编辑的内容都还在。

### 界面

`shisa-settings` 分应用和通用两个入口。应用页显示启动凭证、覆盖情况和最近一次启动失败，可以立即上锁或解锁、调整宽限期和失焦隐藏档位。手工路径、自启动例外和失焦自定义秒数在「高级设置」。通用页是全局宽限期、锁屏是否清理凭证、认证等待上限，底部有诊断卡片，能看最近 64 次启动。

保护路径旁会标出覆盖情况，解析不到或没标记上的显示为未生效。

### 托盘

左键点击 Shisa 托盘打开「恢复窗口」面板，点选应用或用方向键和回车恢复原窗口。右键菜单提供全部上锁、按应用上锁或恢复，以及打开设置。最近一次全部上锁的结果留在托盘提示里。shisad 不在线时托盘图标右下角显示感叹号。托盘没有退出项，要停就用 `systemctl --user stop shisa-agent`。

想要快捷键，在 KDE 系统设置的「快捷键」里加一条命令，填 `shisa lock`。

Shisa 托盘提示显示已隐藏的窗口总数，恢复面板和右键菜单显示各应用的窗口数量。认证后恢复原有窗口，保留未保存的内容。

从托盘恢复时，如果 KWin 无法确认窗口已恢复，Shisa 会通知「窗口恢复未确认」。

### 命令行

```text
shisa status              # 受保护应用、凭证、未生效的路径
shisa lock [<app-id>]     # 全部上锁，或只锁一个
shisa unlock <app-id>
shisa protect <app-id>
shisa unprotect <app-id>
shisa resolve <app-id>
```

### 配置

每个用户一个文件，由 root 拥有，位于 `/etc/shisa/users/<用户名>.toml`：

```toml
[global]
grace_seconds = 300
clear_on_screen_lock = true
prompt_timeout_seconds = 60

[[apps]]
id = "firefox-nightly"
paths = ["/usr/lib/firefox-nightly/firefox"]
grace_seconds = 600
autostart = "prompt"
relock_after_unfocus_seconds = 0
```

`paths` 一般由界面自动填写。宽限期上限 31536000 秒，等待上限 5 到 85 秒，每个受保护应用至少保留一条路径。

## 原理

- `shisad` 以 root 运行，在保护路径的 inode 上挂 fanotify exec 许可标记。被拦的进程停在 `execve` 里，等 shisad 回答放行或 `EPERM`。
- 保护路径从 desktop 文件解析，原生程序、包装脚本、Flatpak 部署入口都能标记。
- 没有解锁凭证时 shisad 走 polkit 认证，密码在桌面认证代理里输入，shisad 既不接触也不保存。
- 认证通过发一张解锁凭证，宽限期内再启动不再询问。宽限期为 0 时只放行当次。
- 屏幕锁定时清空凭证（通用页可关），KWin 脚本把已打开的受保护窗口隐藏。恢复走托盘、设置页的解锁或重启应用，三条都要重新认证。
- 可以用「失焦后隐藏」按应用设置：窗口失焦超过指定秒数就收起，时间没到切回来不隐藏。
- 包升级替换文件后，shisad 每 30 秒核对 inode，pacman hook 在事务结束后立即重载。

认证用单独的 `shisa-lock` 账号。输错、取消或超时会计入 polkit 的 PAM 失败计数，连续 3 次后应用锁暂停约 10 分钟，不影响登录和 sudo。管理员用 `sudo faillock --user shisa-lock --reset` 清零。

## 限制

- 共用运行时的应用只覆盖包装脚本。
- `NoDisplay` 的应用不出现在设置列表里。已受保护的会以 id 出现，能取消保护，但看不到显示名和图标。
- 不是多用户隔离的替代。

未验证：`pkttyagent` 在 SSH 会话里的提示（只在 Konsole 里验证过）。密码未设置时的顶部提示只在预览里看过。多用户隔离没测，回归只跑单账号。

## 开发

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
cargo build --release
tests/gate.sh && tests/auth.sh && tests/settings.sh
```

回归脚本需要 sudo，跑在私有总线上，用 `tests/fake-polkit` 代替 polkitd，不碰 `/etc`、`/run` 和真实密码。

改设置界面前端后先构建前端，再编译 Rust 壳：

```sh
(cd gui/shisa-settings && npm run build)
(cd gui/shisa-settings/src-tauri && cargo build --release --features custom-protocol)
```

打包目录不在仓库里，单独维护。

## 许可证

GNU General Public License v3.0 或更新版本，全文见 [LICENSE](LICENSE)。

Copyright (C) 2026 RiverOnVenus
