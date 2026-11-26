#!/usr/bin/env bash
# Regression test: settings changes through the bus.
#
# Covers `shisa protect`/`unprotect`, `SetOption`, the written config file and
# `shisa resolve`. Every change is authenticated on its own: a ticket does not
# stand in for it. A private dbus-daemon and tests/fake-polkit are used, so no
# real password is involved. Throwaway files live under $W, and T3 protects
# and unprotects /usr/bin/kwrite to exercise the desktop entry resolution. It
# never launches KWrite.
set -u

HERE="$(cd "$(dirname "$0")/.." && pwd)"
SHISAD="$HERE/target/release/shisad"
CLI="$HERE/target/release/shisa"
FAKE="$HERE/target/release/fake-polkit"
W="${TMPDIR:-$HOME/.cache/agent-scratch}/shisa-settings"
LOG="$W/shisad.log"
OUT="$W/results.txt"
ME="$(id -un)"
APP=org.kde.kwrite
FAILED=0

for b in "$SHISAD" "$CLI"; do
  [ -x "$b" ] || { echo "build first: cargo build --release"; exit 1; }
done
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

cat > "$W/user.toml" <<EOF
[global]
grace_seconds = 300
prompt_timeout_seconds = 5
EOF

DEST="io.github.RiverOnVenus.Shisa"
PATH_="/io/github/RiverOnVenus/Shisa/Manager"
IFACE="io.github.RiverOnVenus.Shisa.Manager"
gcall() { gdbus call --address "$DBUS_SYSTEM_BUS_ADDRESS" --dest "$DEST" --object-path "$PATH_" --method "$IFACE.$1" "${@:2}"; }

say "== start private bus, fake polkit and shisad =="
control deny
start_bus
start_fake
trap cleanup EXIT
start_shisad

say "== T0 GlobalConfig reports the file =="
gcall GlobalConfig > "$W/global.txt" 2>&1
grep -q 'uint64 300, true, uint64 5' "$W/global.txt" && say "  ok    300, true, 5" \
  || { say "  FAIL  unexpected answer: $(cat "$W/global.txt")"; FAILED=$((FAILED + 1)); }

say "== T1 resolve needs no authentication =="
expect 0 "shisa resolve $APP" "$CLI" resolve "$APP"
grep -q '/usr/bin/kwrite' "$OUT" && say "  ok    path resolved" \
  || { say "  FAIL  no /usr/bin/kwrite in the output"; FAILED=$((FAILED + 1)); }

say "== T2 a refused change writes nothing =="
expect 1 "protect refused by polkit" "$CLI" protect "$APP"
# The daemon keeps the config directory at 0700, so the file has to be read
# through sudo even when it does not exist yet.
if sudo test -f "$W/etc/$ME.toml" && sudo grep -q "id = \"$APP\"" "$W/etc/$ME.toml"; then
  say "  FAIL  config written without authentication"
  FAILED=$((FAILED + 1))
else
  say "  ok    no config written"
fi

say "== T3 an allowed change is written as root =="
control allow
before=$(checks)
expect 0 "protect $APP" timeout 30 "$CLI" protect "$APP"
[ "$(( $(checks) - before ))" -eq 1 ] && say "  ok    one polkit check" \
  || { say "  FAIL  expected one check"; FAILED=$((FAILED + 1)); }
if "$CLI" status > "$W/status.txt" 2>>"$OUT"; then
  say "  ok    status (rc=0)"
else
  say "  FAIL  status"; FAILED=$((FAILED + 1))
fi
grep -q "$APP" "$W/status.txt" && say "  ok    app listed" \
  || { say "  FAIL  app not listed"; FAILED=$((FAILED + 1)); }
sudo test -f "$W/etc/$ME.toml" && say "  ok    config file written" \
  || { say "  FAIL  config file missing"; FAILED=$((FAILED + 1)); }
sudo stat -c '%a %U' "$W/etc/$ME.toml" | grep -q '^600 root' && say "  ok    mode 600 owned by root" \
  || { say "  FAIL  wrong owner or mode: $(sudo stat -c '%a %U' "$W/etc/$ME.toml")"; FAILED=$((FAILED + 1)); }
sudo stat -c '%a' "$W/etc" | grep -q '^700$' && say "  ok    config directory mode 700" \
  || { say "  FAIL  wrong directory mode: $(sudo stat -c '%a' "$W/etc")"; FAILED=$((FAILED + 1)); }
sudo grep -q "id = \"$APP\"" "$W/etc/$ME.toml" && sudo grep -q '/usr/bin/kwrite' "$W/etc/$ME.toml" \
  && say "  ok    app and its path are in the file" \
  || { say "  FAIL  file content unexpected"; sudo cat "$W/etc/$ME.toml" | tee -a "$OUT"; FAILED=$((FAILED + 1)); }
tail -4 "$OUT" | grep -q "locked *$APP" && say "  ok    approving the change left no ticket" \
  || { grep -q "locked *$APP" "$W/status.txt" && say "  ok    approving the change left no ticket" \
       || { say "  FAIL  the app is unlocked after a settings change"; FAILED=$((FAILED + 1)); }; }

say "== T4 a ticket does not approve a waiting change =="
control deny "delay 8"
timeout 15 "$CLI" unprotect "$APP" >>"$OUT" 2>&1 &
WAITING=$!
sleep 1
control allow
expect 0 "unlock $APP while the change waits" unlock "$APP"
sleep 1
sudo grep -q "id = \"$APP\"" "$W/etc/$ME.toml" && say "  ok    app still protected" \
  || { say "  FAIL  the unlock approved the waiting unprotect"; FAILED=$((FAILED + 1)); }
wait $WAITING; rc=$?
[ "$rc" -ne 0 ] && say "  ok    the waiting change did not succeed (rc=$rc)" \
  || { say "  FAIL  the waiting change succeeded"; FAILED=$((FAILED + 1)); }

say "== T4b with a ticket in hand the next change still asks =="
control allow
before=$(checks)
expect 0 "unprotect $APP" timeout 30 "$CLI" unprotect "$APP"
[ "$(( $(checks) - before ))" -eq 1 ] && say "  ok    one polkit check for the change" \
  || { say "  FAIL  expected one check"; FAILED=$((FAILED + 1)); }
sudo grep -q "id = \"$APP\"" "$W/etc/$ME.toml" \
  && { say "  FAIL  app still in the file"; FAILED=$((FAILED + 1)); } \
  || say "  ok    app removed from the file"
if "$CLI" status > "$W/status.txt" 2>>"$OUT"; then
  say "  ok    status (rc=0)"
else
  say "  FAIL  status"; FAILED=$((FAILED + 1))
fi
grep -q "$APP" "$W/status.txt" \
  && { say "  FAIL  app still listed"; FAILED=$((FAILED + 1)); } \
  || say "  ok    app no longer listed"

say "== T5 SetOption follows the same rule =="
control allow
expect 0 "set the global grace" gcall SetOption "" grace_seconds "<uint64 42>"
gcall GlobalConfig > "$W/global.txt" 2>&1
grep -q 'uint64 42,' "$W/global.txt" && say "  ok    grace changed to 42" \
  || { say "  FAIL  unexpected answer: $(cat "$W/global.txt")"; FAILED=$((FAILED + 1)); }
control deny
gcall SetOption "" grace_seconds "<uint64 99>" >>"$OUT" 2>&1 \
  && { say "  FAIL  a refused option was written"; FAILED=$((FAILED + 1)); } \
  || say "  ok    refused option rejected"
gcall GlobalConfig > "$W/global.txt" 2>&1
grep -q 'uint64 42,' "$W/global.txt" && say "  ok    grace still 42" \
  || { say "  FAIL  the refused value landed: $(cat "$W/global.txt")"; FAILED=$((FAILED + 1)); }

say "== done: $FAILED failed. logs follow =="
printf -- '--- shisad ---\n' >> "$OUT"; cat "$LOG" >> "$OUT"
printf -- '--- fake polkit ---\n' >> "$OUT"; cat "$W/fake.log" >> "$OUT"
echo "results: $OUT"
exit $((FAILED > 0))
