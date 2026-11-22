#!/usr/bin/env bash
# Regression test for the exec gate, tickets and instances.
#
# Run as the normal user from a terminal inside the Plasma session, after
# `cargo build --release`. It calls sudo to install a throwaway config and to
# start shisad, then drives every test unprivileged.
#
# Everything runs on a private dbus-daemon with tests/fake-polkit standing in
# for polkitd, so no real password is involved and nothing under /etc or /run
# is touched. Only throwaway files under $W plus /usr/bin/kwrite are marked.
# Close KWrite before running.
set -u

HERE="$(cd "$(dirname "$0")/.." && pwd)"
SHISAD="$HERE/target/release/shisad"
CLI="$HERE/target/release/shisa"
FAKE="$HERE/target/release/fake-polkit"
W="${TMPDIR:-$HOME/.cache/agent-scratch}/shisa-gate"
LOG="$W/shisad.log"
OUT="$W/results.txt"
ME="$(id -un)"
KWRITE=/usr/bin/kwrite
FAILED=0

[ -x "$SHISAD" ] && [ -x "$CLI" ] || { echo "build first: cargo build --release"; exit 1; }
[ "$(id -u)" -ne 0 ] || { echo "run as the normal user, not root"; exit 1; }
rm -rf "$W"; mkdir -p "$W"
# The assertions below match the English interface. The saved language choice
# lives under XDG_CONFIG_HOME, so point it at the test directory and force the
# C locale instead of depending on the caller's session.
export LC_ALL=C
export XDG_CONFIG_HOME="$W/config"
mkdir -p "$XDG_CONFIG_HOME"
: > "$OUT"

. "$HERE/tests/lib.sh"
build_helper
[ -x "$FAKE" ] || { echo "fake polkit missing"; exit 1; }

# Targets. A shell copy lets a protected program start children.
cp /usr/bin/sleep "$W/target-bin"
cp -L /usr/bin/sh "$W/target-sh"
cp /usr/bin/sleep "$W/cool-bin"
cp "$W/target-bin" "$W/copy-bin"
ln -s "$W/target-bin" "$W/link-sym"
ln "$W/target-bin" "$W/link-hard"
printf '#!/bin/sh\necho "script ran: $*"\n' > "$W/target.sh"; chmod +x "$W/target.sh"

cat > "$W/user.toml" <<EOF
[global]
grace_seconds = 300
prompt_timeout_seconds = 20

[[apps]]
id = "gate-app"
paths = ["$W/target-sh", "$W/target-bin"]
grace_seconds = 4

[[apps]]
id = "gate-once"
paths = ["$W/target.sh"]
grace_seconds = 0

[[apps]]
id = "gate-cool"
paths = ["$W/cool-bin"]

[[apps]]
id = "org.kde.kwrite"
paths = ["$KWRITE"]
grace_seconds = 30
EOF

say "== start private bus, fake polkit and shisad =="
control deny
start_bus
start_fake
trap cleanup EXIT
start_shisad

say "== T1 unprotected copy runs =="
expect 0 "copy of a protected binary" "$W/copy-bin" 0.1

say "== T2 no ticket: every entry is denied with EPERM (rc 126) =="
expect 126 "binary" "$W/target-bin" 0.1
expect 126 "symlink" "$W/link-sym" 0.1
expect 126 "hardlink" "$W/link-hard" 0.1
expect 126 "script" "$W/target.sh" a b
grep -q 'Operation not permitted' "$OUT" && say "  ok    error text is EPERM" \
  || { say "  FAIL  no 'Operation not permitted' in output"; FAILED=$((FAILED + 1)); }

say "== T3 a refused authentication gives no ticket =="
expect 1 "unlock refused by polkit" unlock gate-app
expect 126 "still denied" "$W/target-bin" 0.1
expect 1 "unlock of an app that is not protected" unlock no-such-app

say "== T4 shisa unlock, then launch =="
control allow
expect 0 "unlock" unlock gate-app
control deny
expect 0 "binary runs, arguments intact" "$W/target-bin" 0.1
expect 0 "symlink runs" "$W/link-sym" 0.1
expect 126 "ticket does not cover another app" "$W/cool-bin" 0.1

say "== T5 instance outlives the ticket (grace is 4s) =="
"$W/target-sh" -c "sleep 6; /usr/bin/env '$W/target-bin' 0.1; echo \"via-env rc=\$?\"; exec '$W/target-bin' 0.1" \
  > "$W/instance.out" 2>&1 &
INSTANCE=$!
sleep 5
expect 126 "new launch after the grace period" "$W/target-bin" 0.1
wait $INSTANCE; rc=$?
cat "$W/instance.out" >> "$OUT"
[ "$rc" -eq 0 ] && grep -q 'via-env rc=0' "$W/instance.out" \
  && say "  ok    descendant and self re-exec allowed without a ticket" \
  || { say "  FAIL  instance: rc=$rc, $(cat "$W/instance.out")"; FAILED=$((FAILED + 1)); }

say "== T6 grace 0: the ticket covers one launch =="
control allow
expect 0 "unlock" unlock gate-once
control deny
expect 0 "first launch" "$W/target.sh" one
expect 126 "second launch" "$W/target.sh" two
expect 0 "sh <script> is not an exec open, known gap" sh "$W/target.sh" via-sh

say "== T7 refused attempts do not block a later authentication =="
control deny
expect 126 "refused launch 1" "$W/target-bin" 0.1
expect 126 "refused launch 2" "$W/target-bin" 0.1
expect 1 "refused unlock" unlock gate-cool
control allow
expect 0 "a later unlock still works" unlock gate-cool
expect 0 "and its ticket releases the launch" "$W/cool-bin" 0.1

say "== T8 file replaced by rename, then SIGHUP re-marks it =="
control deny
cp /usr/bin/sleep "$W/target-bin.new" && mv "$W/target-bin.new" "$W/target-bin"
expect 0 "new inode is unmarked until reload" "$W/target-bin" 0.1
sudo -n pkill -HUP -f "^$SHISAD --config-dir $W/etc" && sleep 1
expect 126 "denied again after reload" "$W/target-bin" 0.1
expect 0 "old inode is no longer protected" "$W/link-hard" 0.1

say "== T8b the 30 s check re-marks without a signal =="
cp /usr/bin/sleep "$W/target-bin.new" && mv "$W/target-bin.new" "$W/target-bin"
expect 0 "replaced file runs before the check" "$W/target-bin" 0.1
sleep 35
expect 126 "denied again after the 30 s check" "$W/target-bin" 0.1

say "== T9 a config change takes effect on SIGHUP, a broken one is ignored =="
sed 's/^id = "gate-cool"/id = "gate-renamed"/' "$W/user.toml" > "$W/user2.toml"
sudo install -o root -g root -m 644 "$W/user2.toml" "$W/etc/$ME.toml"
sudo -n pkill -HUP -f "^$SHISAD --config-dir $W/etc" && sleep 1
expect 1 "old app id is gone" unlock gate-cool
control allow
expect 0 "new app id unlocks" unlock gate-renamed
control deny
expect 0 "and launches" "$W/cool-bin" 0.1
echo 'this is not toml' > "$W/broken.toml"
sudo install -o root -g root -m 644 "$W/broken.toml" "$W/etc/$ME.toml"
sudo -n pkill -HUP -f "^$SHISAD --config-dir $W/etc" && sleep 1
expect 126 "protection survives a broken config" "$W/target-bin" 0.1
sudo install -o "$ME" -m 644 "$W/user.toml" "$W/etc/$ME.toml"
sudo -n pkill -HUP -f "^$SHISAD --config-dir $W/etc" && sleep 1
expect 126 "a config not owned by root is ignored" "$W/target-bin" 0.1
grep -q 'must be owned by root' "$LOG" && say "  ok    ownership error logged" \
  || { say "  FAIL  ownership error not logged"; FAILED=$((FAILED + 1)); }

say "== T10 Plasma launch path (kioclient) =="
if pgrep -x kwrite >/dev/null; then
  say "  SKIP  kwrite already running"
elif env -u DBUS_SYSTEM_BUS_ADDRESS timeout 3 gdbus call --system \
    --dest io.github.RiverOnVenus.Shisa \
    --object-path /io/github/RiverOnVenus/Shisa/Manager \
    --method io.github.RiverOnVenus.Shisa.Manager.ListApps 2>/dev/null \
    | grep -q org.kde.kwrite; then
  say "  SKIP  the installed shisad already protects KWrite"
else
  control deny
  kioclient exec /usr/share/applications/org.kde.kwrite.desktop >>"$OUT" 2>&1 &
  sleep 4
  if pgrep -x kwrite >/dev/null; then say "  FAIL  kwrite started without a ticket"; FAILED=$((FAILED + 1)); pkill -x kwrite
  else say "  ok    icon launch denied"; fi
  control allow
  expect 0 "unlock" unlock org.kde.kwrite
  control deny
  kioclient exec /usr/share/applications/org.kde.kwrite.desktop >>"$OUT" 2>&1 &
  sleep 4
  if pgrep -x kwrite >/dev/null; then say "  ok    icon launch works after unlock"
  else say "  FAIL  kwrite did not start after unlock"; FAILED=$((FAILED + 1)); fi
  pkill -x kwrite
fi

say "== T11 stopping shisad removes the protection =="
stop_shisad
expect 0 "protected binary runs once shisad is gone" "$W/target-bin" 0.1

say "== done: $FAILED failed. daemon log follows =="
cat "$LOG" >> "$OUT"
echo "results: $OUT"
exit $((FAILED > 0))
