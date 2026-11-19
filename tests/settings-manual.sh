#!/usr/bin/env bash
# Manual check of the settings window and the real polkit dialog.
#
# polkit runs on the real system bus, so the test daemon owns the Shisa name
# there and the installed shisad is stopped for the duration. The config lives
# under $W. The dialog asks for your real password.
set -u

HERE="$(cd "$(dirname "$0")/.." && pwd)"
SHISAD="$HERE/target/release/shisad"
SETTINGS="$HERE/gui/shisa-settings/src-tauri/target/release/shisa-settings"
W="${TMPDIR:-$HOME/.cache/agent-scratch}/shisa-settings-manual"
ME="$(id -un)"
unset DBUS_SYSTEM_BUS_ADDRESS

[ -x "$SHISAD" ] || { echo "build first: cargo build --release"; exit 1; }
[ -x "$SETTINGS" ] || { echo "build the settings app first"; exit 1; }
[ "$(id -u)" -ne 0 ] || { echo "run as the normal user, not root"; exit 1; }
if pgrep -x shisa-settings >/dev/null; then
  echo "先关掉已打开的设置窗口：单实例会把新进程转到旧窗口"; exit 1
fi

rm -rf "$W"; mkdir -p "$W"

WAS_ACTIVE=no
if systemctl is-active --quiet shisad; then
  WAS_ACTIVE=yes
  sudo systemctl stop shisad || exit 1
fi
SETTINGS_PID=""
restore() {
  kill ${SETTINGS_PID:-} 2>/dev/null
  sudo -n pkill -f "^$SHISAD --config-dir $W/etc" 2>/dev/null
  sudo -n rm -rf "$W/etc"
  [ "$WAS_ACTIVE" = yes ] && sudo systemctl start shisad
}
trap restore EXIT
trap 'restore; exit 130' INT TERM

sudo -v || exit 1
sudo install -d -o root -g root -m 755 "$W/etc"
sudo "$SHISAD" --config-dir "$W/etc" --lifetime 1800 --verbose > "$W/shisad.log" 2>&1 &
for _ in $(seq 25); do grep -q 'shisad ready' "$W/shisad.log" 2>/dev/null && break; sleep 0.2; done
grep -q 'shisad ready' "$W/shisad.log" || { echo "shisad 没起来，见 $W/shisad.log"; exit 1; }

"$SETTINGS" > "$W/settings.log" 2>&1 &
SETTINGS_PID=$!
sleep 2
DEMO_BIN="$W/demo-bin"
printf '#!/bin/sh\n' > "$DEMO_BIN"
chmod +x "$DEMO_BIN"

cat <<EOT

设置窗口已经打开。系统里的 shisad 已暂停，结束时自动恢复。日志：
  shisad    $W/shisad.log
  settings  $W/settings.log
  配置      $W/etc/$ME.toml（改动后会出现在这里）

请依次检查：

1. 左侧列表有应用和图标，搜索框能过滤。
2. 选中 KWrite，打开「启动前要求认证」。出现 KDE 的 polkit 对话框，正文是
   「修改 KWrite 需要验证身份」，输入应用锁密码（新版包是 shisa-lock 的密码，
   没装新版时是你的登录密码）。通过后开关保持打开，左侧该应用出现「已保护」标记。
3. 再关掉开关。设置改动每次都要认证，解锁凭证不能代替，所以还会弹框。
   这是 DESIGN 7.2 的行为。
4. 打开开关让它重新受保护，然后改动宽限期（例如 600），观察配置文件。
5. 在「保护路径」里加一条存在的绝对路径（$DEMO_BIN），观察它出现在列表和文件里，
   再移除。只剩一条路径时「移除」会禁用，要放开应用应关掉保护开关。
   再加一条不存在的 /opt/demo 和一个目录 /usr/share。认证后应报错，列表和文件都不变。
6. 点「全部上锁」，确认没有报错。
7. 弹框时点「取消」，界面应显示取消的错误提示，设置值回到原样。
8. 点左侧底部的「通用」，三个控件显示 300 秒、锁屏清除已通过的认证、60 秒。
   改默认宽限期，弹框正文应是「修改 Shisa 设置 需要验证身份」，通过后控件保持新值。
   改等待上限后，再看配置文件的 [global] 部分。
9. 选一个应用 id 很长的，打开保护。弹框里的名字是应用的显示名。

Ctrl-C 结束并清理。
EOT

while true; do sleep 60; done
