#!/usr/bin/env bash
# Manual check of the real polkit dialog.
#
# polkit runs on the real system bus, so the test daemon has to own the Shisa
# name there. That means the installed shisad is stopped for the duration and
# started again on exit. The test daemon reads a throwaway config under $W, so
# /etc is not changed. The dialog asks for your real password.
set -u

HERE="$(cd "$(dirname "$0")/.." && pwd)"
SHISAD="$HERE/target/release/shisad"
CLI="$HERE/target/release/shisa"
W="${TMPDIR:-$HOME/.cache/agent-scratch}/shisa-auth-manual"
ME="$(id -un)"
unset DBUS_SYSTEM_BUS_ADDRESS

[ -x "$SHISAD" ] && [ -x "$CLI" ] || { echo "build first: cargo build --release"; exit 1; }
[ "$(id -u)" -ne 0 ] || { echo "run as the normal user, not root"; exit 1; }
if pgrep -x kwrite >/dev/null; then
  echo "先关掉 KWrite"; exit 1
fi

rm -rf "$W"; mkdir -p "$W"
cat > "$W/user.toml" <<EOT
[global]
grace_seconds = 300
prompt_timeout_seconds = 60

[[apps]]
id = "org.kde.kwrite"
paths = ["/usr/bin/kwrite"]
EOT

WAS_ACTIVE=no
if systemctl is-active --quiet shisad; then
  WAS_ACTIVE=yes
  sudo systemctl stop shisad || exit 1
fi
restore() {
  sudo -n pkill -f "^$SHISAD --config-dir $W/etc" 2>/dev/null
  sudo -n rm -rf "$W/etc"
  [ "$WAS_ACTIVE" = yes ] && sudo systemctl start shisad
}
trap restore EXIT
trap 'restore; exit 130' INT TERM

sudo -v || exit 1
sudo install -d -o root -g root -m 755 "$W/etc"
sudo install -o root -g root -m 644 "$W/user.toml" "$W/etc/$ME.toml"
sudo "$SHISAD" --config-dir "$W/etc" --lifetime 1800 --verbose > "$W/shisad.log" 2>&1 &
for _ in $(seq 25); do grep -q 'shisad ready' "$W/shisad.log" 2>/dev/null && break; sleep 0.2; done
grep -q 'shisad ready' "$W/shisad.log" || { echo "shisad 没起来，见 $W/shisad.log"; exit 1; }

cat <<EOT

准备就绪。系统里的 shisad 已暂停，结束时自动恢复。日志：
  shisad  $W/shisad.log

每步点一次 KWrite 图标（或运行
  kioclient exec /usr/share/applications/org.kde.kwrite.desktop）：

1. 启动 KWrite。KDE 的 polkit 对话框应出现，正文是「打开 KWrite 需要验证身份」，
   下面写「正在作为 ... 进行身份验证」。新版包里这里是 shisa-lock，输入应用锁密码。
   没装新版时这里是你自己的账号，输入登录密码。
2. 输错一次密码，对话框应提示失败并允许重试。记下失败计数：
     sudo faillock --user shisa-lock
   没有 shisa-lock 时看 `sudo faillock --user $ME`。
3. 输对密码，KWrite 启动。再点一次 KWrite，宽限期内不应再弹框。
4. 运行 `$CLI lock org.kde.kwrite`，再点 KWrite。弹框时点「取消」，KWrite 不应启动。
5. 再点 KWrite，什么都不输，等 60 秒。对话框应消失，KWrite 不应启动。
6. 在终端运行 `$CLI unlock org.kde.kwrite`。应由 pkttyagent 在终端里提示应用锁密码，
   通过后再点 KWrite 不应弹框。

Ctrl-C 结束并清理。
EOT

while true; do sleep 60; done
