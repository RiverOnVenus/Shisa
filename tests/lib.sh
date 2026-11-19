#!/usr/bin/env bash
# Shared helpers for the regression tests. The caller sets HERE, W, LOG, OUT,
# ME, SHISAD, CLI and FAKE, then sources this file.

say() { echo "$*" | tee -a "$OUT"; }

# expect <rc> <description> <command...>
expect() {
  local want=$1 what=$2 rc; shift 2
  "$@" >>"$OUT" 2>&1; rc=$?
  if [ "$rc" -eq "$want" ]; then say "  ok    $what (rc=$rc)"
  else say "  FAIL  $what: rc=$rc, wanted $want"; FAILED=$((FAILED + 1)); fi
}

# What the fake polkit answers next. A second line "delay N" waits N seconds.
control() { printf '%s\n' "$@" > "$W/control"; }

build_helper() {
  CARGO_TARGET_DIR="$HERE/target" cargo build --release --offline \
    --manifest-path "$HERE/tests/fake-polkit/Cargo.toml" >>"$OUT" 2>&1 \
    || { echo "cannot build tests/fake-polkit"; exit 1; }
}

start_bus() {
  cat > "$W/bus.conf" <<EOF
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>system</type>
  <listen>unix:path=$W/bus.sock</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow user="*"/>
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
EOF
  dbus-daemon --config-file="$W/bus.conf" --nofork > "$W/bus.log" 2>&1 &
  BUS_PID=$!
  for _ in $(seq 50); do [ -S "$W/bus.sock" ] && break; sleep 0.1; done
  [ -S "$W/bus.sock" ] || { echo "private bus did not start"; exit 1; }
  export DBUS_SYSTEM_BUS_ADDRESS="unix:path=$W/bus.sock"
}

start_fake() {
  FAKE_POLKIT_CONTROL="$W/control" "$FAKE" > "$W/fake.log" 2>&1 &
  FAKE_PID=$!
  for _ in $(seq 50); do grep -q 'fake polkit ready' "$W/fake.log" && break; sleep 0.1; done
  grep -q 'fake polkit ready' "$W/fake.log" || { echo "fake polkit did not start"; exit 1; }
}

start_shisad() {
  sudo -v || exit 1
  sudo rm -rf "$W/etc"
  sudo install -d -o root -g root -m 755 "$W/etc"
  sudo install -o root -g root -m 644 "$W/user.toml" "$W/etc/$ME.toml"
  sudo env DBUS_SYSTEM_BUS_ADDRESS="$DBUS_SYSTEM_BUS_ADDRESS" \
    "$SHISAD" --config-dir "$W/etc" --lifetime 900 --verbose > "$LOG" 2>&1 &
  for _ in $(seq 25); do grep -q 'shisad ready' "$LOG" 2>/dev/null && break; sleep 0.2; done
  grep -q 'shisad ready' "$LOG" || { say "shisad did not start:"; tee -a "$OUT" < "$LOG"; exit 1; }
}

stop_shisad() { sudo -n pkill -TERM -f "^$SHISAD --config-dir $W/etc"; sleep 1; }

# No controlling terminal under setsid, so the CLI does not start pkttyagent.
unlock() { setsid -w "$CLI" unlock "$1"; }

checks() { grep -c '^check ' "$W/fake.log" 2>/dev/null || true; }

cleanup() {
  kill ${BUS_PID:-} ${FAKE_PID:-} 2>/dev/null
  sudo -n pkill -f "^$SHISAD --config-dir $W/etc" 2>/dev/null
  sudo -n rm -rf "$W/etc"
}
