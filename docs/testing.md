# 测试与验收


## Rust 与设置界面

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets
(cd gui/shisa-settings && npm ci && npm run build)
(cd gui/shisa-settings/src-tauri && cargo build --features custom-protocol)
```

## 守护进程回归

改动 `shisad` 后，另外运行 `tests/gate.sh`、`tests/auth.sh` 和 `tests/settings.sh`。三个脚本都需要 sudo，执行前先获得同意。测试守护进程应有存活上限，使用 `tests/fake-polkit`，只标记临时文件或 KWrite，避开正在使用的浏览器、终端和 shell。脚本里不得出现真实密码。分别记录 fanotify 和 polkit 实际覆盖的检查。

## 窗口与认证验收

1. 在隔离 KWin 会话打开一个受保护窗口和一个普通窗口。上锁后确认前者收起、后者可用。重复上锁，验证回执继续反映实际窗口状态。
2. 验证无匹配窗口，以及代理或 KWin 未响应时的「窗口状态未确认」。测试失焦后重新聚焦取消计时和超时后隐藏。
3. 按 [Arch 打包指南](packaging.md)安装测试包并启动新版本。在真实 Wayland 会话人工确认托盘点击、认证取消、恢复原窗口及其未保存内容、前台激活、任务栏和概览。xdotool 无法向真实 Wayland 窗口注入点击。
4. 换版本验证设置窗口时，先关闭窗口，清理它的 WebKit 缓存 `$XDG_RUNTIME_DIR/shisa-settings`，再重新打开。
