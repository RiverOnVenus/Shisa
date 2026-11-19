#!/usr/bin/env bash
# Regression test: the polkit authentication flow.
#
# Run as the normal user from a terminal inside the Plasma session, after
# `cargo build --release`. Everything runs on a private dbus-daemon with
# tests/fake-polkit standing in for polkitd, so no real password is involved.
#
# Close KWrite before running. Only throwaway files under $W are marked.
set -u

HERE="$(cd "$(dirname "$0")/.." && pwd)"
SHISAD="$HERE/target/release/shisad"
CLI="$HERE/target/release/shisa"
FAKE="$HERE/target/release/fake-polkit"
W="${TMPDIR:-$HOME/.cache/agent-scratch}/shisa-auth"
LOG="$W/shisad.log"
OUT="$W/results.txt"
ME="$(id -un)"
FAILED=0

for b in "$SHISAD" "$CLI"; do
  [ -x "$b" ] || { echo "build first: cargo build --release"; exit 1; }
done
[ "$(id -u)" -ne 0 ] || { echo "run as the normal user, not root"; exit 1; }
rm -rf "$W"; mkdir -p "$W"
: > "$OUT"

. "$HERE/tests/lib.sh"
build_helper
[ -x "$FAKE" ] || { echo "fake polkit missing"; exit 1; }

# The window-state calls of the window layer. It no longer authenticates on
# its own; the tray and the exec gate are the only places a box comes from.
ws() {
  gdbus call --address "$DBUS_SYSTEM_BUS_ADDRESS" \
    --dest io.github.RiverOnVenus.Shisa \
    --object-path /io/github/RiverOnVenus/Shisa/Manager \
    --method io.github.RiverOnVenus.Shisa.Manager.WindowStates
}
rs() {
  gdbus call --address "$DBUS_SYSTEM_BUS_ADDRESS" \
    --dest io.github.RiverOnVenus.Shisa \
    --object-path /io/github/RiverOnVenus/Shisa/Manager \
    --method io.github.RiverOnVenus.Shisa.Manager.RelockSeconds
}
recent() {
  gdbus call --address "$DBUS_SYSTEM_BUS_ADDRESS" \
    --dest io.github.RiverOnVenus.Shisa \
    --object-path /io/github/RiverOnVenus/Shisa/Manager \
    --method io.github.RiverOnVenus.Shisa.Manager.RecentLaunches
}
diag() {
  gdbus call --address "$DBUS_SYSTEM_BUS_ADDRESS" \
    --dest io.github.RiverOnVenus.Shisa \
    --object-path /io/github/RiverOnVenus/Shisa/Manager \
    --method io.github.RiverOnVenus.Shisa.Manager.Diagnostics
}

cp /usr/bin/sleep "$W/target-bin"
cp /usr/bin/sleep "$W/other-bin"
cat > "$W/user.toml" <<EOF
[global]
grace_seconds = 300
prompt_timeout_seconds = 5

[[apps]]
id = "auth-app"
paths = ["$W/target-bin"]
grace_seconds = 0
relock_after_unfocus_seconds = 5

[[apps]]
id = "auth-other"
paths = ["$W/other-bin"]
EOF

say "== start private bus, fake polkit and shisad =="
control deny
start_bus
start_fake
trap cleanup EXIT
start_shisad

say "== T1 status before authentication =="
expect 0 "shisa status" "$CLI" status
grep -q 'lock *auth-app' "$OUT" && say "  ok    auth-app reported locked" \
  || { say "  FAIL  auth-app not reported as locked"; FAILED=$((FAILED + 1)); }

say "== T2 polkit refuses: the launch is denied =="
before=$(checks)
t0=$(date +%s.%N)
expect 126 "exec denied" timeout 10 "$W/target-bin" 0.1
t1=$(date +%s.%N)
[ "$(( $(checks) - before ))" -eq 1 ] && say "  ok    one polkit check" \
  || { say "  FAIL  expected one polkit check, got $(( $(checks) - before ))"; FAILED=$((FAILED + 1)); }
awk -v a="$t0" -v b="$t1" 'BEGIN{exit !(b-a < 3)}' \
  && say "  ok    denial came back quickly" \
  || { say "  FAIL  waited for the full timeout"; FAILED=$((FAILED + 1)); }

say "== T2b recent launches and diagnostics =="
recent > "$W/recent.txt" 2>&1
grep -q "'denied'" "$W/recent.txt" && say "  ok    the denial is recorded" \
  || { say "  FAIL  no denied record: $(cat "$W/recent.txt")"; FAILED=$((FAILED + 1)); }
grep -q 'uint64 1, uint64 0, uint64 1' "$W/recent.txt" && say "  ok    one request, none allowed, one denied" \
  || { say "  FAIL  unexpected totals: $(cat "$W/recent.txt")"; FAILED=$((FAILED + 1)); }
diag > "$W/diag.txt" 2>&1
grep -q "'0\.1\.0'" "$W/diag.txt" && say "  ok    the version is reported" \
  || { say "  FAIL  no version: $(cat "$W/diag.txt")"; FAILED=$((FAILED + 1)); }
grep -q 'uint64 2, uint64 2' "$W/diag.txt" && say "  ok    two apps and two marked paths" \
  || { say "  FAIL  unexpected counts: $(cat "$W/diag.txt")"; FAILED=$((FAILED + 1)); }
grep -q "$W/etc/$ME.toml" "$W/diag.txt" && say "  ok    the config path is reported" \
  || { say "  FAIL  no config path: $(cat "$W/diag.txt")"; FAILED=$((FAILED + 1)); }

say "== T3 polkit allows: the launch runs =="
control allow
before=$(checks)
expect 0 "exec allowed" timeout 10 "$W/target-bin" 0.1
[ "$(( $(checks) - before ))" -eq 1 ] && say "  ok    one polkit check" \
  || { say "  FAIL  expected one polkit check"; FAILED=$((FAILED + 1)); }

say "== T4 a slow answer is cut off at the wait limit =="
control allow "delay 8"
before=$(checks)
t0=$(date +%s.%N)
expect 126 "denied at the 5 s limit" timeout 20 "$W/target-bin" 0.1
t1=$(date +%s.%N)
elapsed=$(awk -v a="$t0" -v b="$t1" 'BEGIN{printf "%.1f", b-a}')
awk -v e="$elapsed" 'BEGIN{exit !(e >= 4 && e < 9)}' \
  && say "  ok    denied after ${elapsed}s" \
  || { say "  FAIL  denied after ${elapsed}s, expected about 5"; FAILED=$((FAILED + 1)); }
grep -q 'cancel shisa-' "$W/fake.log" && say "  ok    the check was cancelled" \
  || { say "  FAIL  no cancellation reached the fake polkit"; FAILED=$((FAILED + 1)); }
[ "$(( $(checks) - before ))" -eq 1 ] && say "  ok    one polkit check" \
  || { say "  FAIL  expected one polkit check"; FAILED=$((FAILED + 1)); }

say "== T5 two launches of one app share one check =="
control allow "delay 2"
before=$(checks)
"$W/target-bin" 0.1 >>"$OUT" 2>&1 & ONE=$!
"$W/target-bin" 0.1 >>"$OUT" 2>&1 & TWO=$!
wait $ONE; rc1=$?
wait $TWO; rc2=$?
[ "$rc1" -eq 0 ] && [ "$rc2" -eq 0 ] && say "  ok    both launches ran" \
  || { say "  FAIL  launches: $rc1, $rc2"; FAILED=$((FAILED + 1)); }
[ "$(( $(checks) - before ))" -eq 1 ] && say "  ok    one check for both" \
  || { say "  FAIL  expected one check, got $(( $(checks) - before ))"; FAILED=$((FAILED + 1)); }

say "== T6 unlock leaves a ticket, lock drops it =="
control allow
expect 0 "unlock" unlock auth-app
expect 0 "status" "$CLI" status
grep -q 'open *auth-app' "$OUT" && say "  ok    auth-app reported open" \
  || { say "  FAIL  auth-app not reported as open"; FAILED=$((FAILED + 1)); }
expect 0 "lock one app" "$CLI" lock auth-app
expect 0 "status" "$CLI" status
tail -5 "$OUT" | grep -q 'lock *auth-app' && say "  ok    auth-app locked again" \
  || { say "  FAIL  auth-app still open after lock"; FAILED=$((FAILED + 1)); }

say "== T7 window state =="
# Unlock leaves both a ticket and a window permission. Using the ticket for
# a launch must not take the window permission away.
control allow
expect 0 "unlock" unlock auth-app
ws | grep -q 'auth-app' && say "  ok    the window is allowed" \
  || { say "  FAIL  the window state is missing"; FAILED=$((FAILED + 1)); }
rs | grep -q 'auth-app' && say "  ok    the unfocus delay is reported" \
  || { say "  FAIL  the unfocus delay is missing"; FAILED=$((FAILED + 1)); }
expect 0 "launch uses the ticket" "$W/target-bin" 0.1
ws | grep -q 'auth-app' && say "  ok    a used ticket does not hide the window" \
  || { say "  FAIL  the window was hidden when the ticket was used"; FAILED=$((FAILED + 1)); }
expect 0 "lock" "$CLI" lock auth-app
ws | grep -q 'auth-app' && { say "  FAIL  the window state survived the lock"; FAILED=$((FAILED + 1)); } \
  || say "  ok    the lock dropped the window state"

say "== T8 stopping shisad removes the protection =="
stop_shisad
expect 0 "binary runs once shisad is gone" "$W/target-bin" 0.1

say "== done: $FAILED failed. logs follow =="
printf -- '--- shisad ---\n' >> "$OUT"; cat "$LOG" >> "$OUT"
printf -- '--- fake polkit ---\n' >> "$OUT"; cat "$W/fake.log" >> "$OUT"
echo "results: $OUT"
exit $((FAILED > 0))
