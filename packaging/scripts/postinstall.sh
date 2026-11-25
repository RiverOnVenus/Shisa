#!/bin/sh
# Package hook shared by the Fedora RPM and Debian DEB. The Arch package keeps
# its own message in packaging/arch/shisa.install.
set -u

# The DEB passes "configure" and the previously installed version as $2. The
# RPM passes the number of installed packages after the transaction as $1.
# Both forms distinguish a fresh install from an upgrade.
case "${1:-}" in
    configure)
        if [ -n "${2:-}" ]; then upgrade=1; else upgrade=0; fi
        ;;
    abort-upgrade|abort-remove|abort-deconfigure)
        upgrade=1
        ;;
    *)
        if [ "${1:-1}" -gt 1 ] 2>/dev/null; then upgrade=1; else upgrade=0; fi
        ;;
esac

# The sysusers entry creates the password holder. Running it again on upgrade
# is safe, and a failure must not abort the package transaction.
if command -v systemd-sysusers >/dev/null 2>&1; then
    systemd-sysusers /usr/lib/sysusers.d/shisa-lock.conf >/dev/null 2>&1 || true
fi

# Load the new systemd units and D-Bus policy, then let a running daemon
# re-read the protected paths. try-reload-or-restart does nothing when the
# service is stopped, so a first install does not start it.
if command -v systemctl >/dev/null 2>&1; then
    systemctl daemon-reload >/dev/null 2>&1 || true
    # Reloading the bus applies the new policy without a reboot. A first
    # install or a build container has no active bus, so only warn when the
    # bus is running and the reload itself fails.
    if systemctl is-active --quiet dbus.service 2>/dev/null; then
        if ! reload_output=$(systemctl reload dbus.service 2>&1); then
            printf '%s\n' \
                "Warning: the D-Bus policy could not be reloaded." \
                "${reload_output:-No error output.}" \
                "Restart the computer before relying on the new Shisa permissions."
        fi
    fi
    systemctl try-reload-or-restart shisad.service >/dev/null 2>&1 || true
fi

# A usable password is "P" or "PS". The locked and empty states ("L", "LK",
# "NP") and a missing account all need the hint.
status=$(passwd -S shisa-lock 2>/dev/null | awk '{print $2}')
case "$status" in
    P|PS) ;;
    *)
        cat <<'EOF'
The application-lock password is not set yet:

  sudo passwd shisa-lock

Protected applications cannot authenticate until it is set. The password is
separate from the login password.
EOF
        ;;
esac

if [ "$upgrade" -eq 0 ]; then
    cat <<'EOF'

Start Shisa after a first install:

  sudo systemctl enable --now shisad.service
  systemctl --user daemon-reload
  systemctl --user enable --now shisa-agent.service
EOF
else
    cat <<'EOF'

After an upgrade, restart both services to load the new binaries:

  sudo systemctl restart shisad.service
  systemctl --user restart shisa-agent.service
EOF
fi
