#!/bin/sh
# The package leaves the configuration and the password holder in place, so
# reinstalling keeps both. Print the commands the user can run first, but only
# on removal: RPM %preun passes 1 and DEB prerm passes upgrade for an upgrade.
case "${1:-}" in
    upgrade|deconfigure|failed-upgrade|1) exit 0 ;;
esac

cat <<'EOF'
Shisa's configuration in /etc/shisa and the shisa-lock account are kept.

To stop the services before removing them:

  sudo systemctl disable --now shisad.service
  systemctl --user disable --now shisa-agent.service

To delete the account after removal:

  sudo userdel shisa-lock
EOF
