# Launch gating

## Mechanism

shisad uses fanotify `FAN_OPEN_EXEC_PERM`. The kernel pauses the caller while opening an executable and waits for the daemon's decision. This event has been available since Linux 5.0 and requires `CAP_SYS_ADMIN`.

Marks apply to the inodes of protected files. Unmarked applications launch without involving Shisa. Before marking an open descriptor, shisad checks that it refers to a regular file and excludes shells, interpreters, systemd, and Shisa executables. The check and the mark refer to the same inode even if the path is replaced in between.

Symlinks and hard links to the same inode are covered. A copied binary has a different inode. If the bounded fanotify queue overflows, Linux can allow discarded permission events without a decision. The daemon logs `FAN_Q_OVERFLOW`. See the [threat model](../architecture.md#threat-model).

## Protected path resolution

shisa-core derives protected paths from an app's desktop entry. Settings lists the result and lets the user edit it.

- Native apps use the executable from each `Exec` line, including desktop actions. Resolve it through `PATH` and then resolve the filesystem path.
- Wrapper scripts are marked at the script inode. Executing the wrapper triggers the event. Passing it as a file argument to an interpreter does not.
- Shared runtimes such as Electron are left unmarked to avoid locking unrelated apps. The resolver marks app-specific wrappers and settings reports the coverage gap for direct runtime invocation.
- Flatpak resolution reads the app command from deployment metadata and marks the entry executable. The shared `flatpak` executable stays unmarked. Global options such as `--user` and `--installation` can precede `run`. Stored paths preserve the `current/active` symlinks across updates. Inode marks also apply inside the sandbox's mount namespace.
- For `env VAR=value program`, skip `env` and its assignments when locating the executable.
- Desktop entries that launch the same executable can share a protected path. To distinguish a shared runtime, compare the first argument that is neither an option nor a desktop field code, together with any `env` assignments. Different `app.asar` arguments are one example. Entries shadowed by an earlier data directory for the same desktop ID do not count as separate apps. Entry points that execute different inodes require separate marks.

The resolver reads `XDG_DATA_HOME`, `XDG_DATA_DIRS`, `XDG_CONFIG_HOME`, `PATH`, and the locale (`LC_ALL`, `LC_MESSAGES`, `LANG`) from the user's systemd manager process and falls back to standard directories if the manager is missing. A scan reads at most 8,192 entries and 8 MiB of text. It follows symlinked desktop files and skips symlinked directories. For localized `Name[locale]` it tries `lang_COUNTRY` before `lang`.

The daemon checks protected paths every 30 seconds and refreshes marks when an inode changes or a missing path reappears. A path that remains unavailable does not cause repeated full reloads. This covers Flatpak deployments and manual file replacement. The Arch package also ships a post-transaction hook that reloads a running daemon after a pacman upgrade.

## Launch decisions

Before exec completes, `/proc/<pid>/exe` still identifies the caller's old process image.

1. Look up the app by the executable inode. Allow unknown inodes and apps unprotected for the caller's UID.
2. Allow execution from an existing app instance or when the caller's current image is itself a protected executable of the same app.
3. For a valid ticket, allow the launch and register an instance.
4. Otherwise, suspend the request and ask polkit. Authorization issues a ticket, registers an instance, and resumes the original exec with its original arguments, environment, terminal, and cgroup. Refusal, cancellation, and timeout return `EPERM` to the caller.

Waiting callers sleep in exec and can be killed by a signal. The default wait is 60 seconds. The event loop answers every event and delegates authentication to separate work. Excess pending launches are refused once they reach 64 per app or 256 per user.

On daemon exit the kernel releases pending events and removes marks. systemd uses `Restart=always`, and the tray reports daemon availability. The service uses `Type=notify` and `WatchdogSec=10` with a heartbeat every five seconds to recover from event-loop stalls. Root can temporarily disable launch protection by stopping `shisad` from a TTY.

## Tickets and instances

Tickets are stored in memory by user and app. The grace period starts on authorization. A zero grace period allows the pending launch once. An explicit `shisa unlock` without a pending launch issues a one-use ticket that expires after the configured wait limit.

Manual locking revokes tickets and window allowance. Focus-loss hiding revokes only the window allowance, so the ticket and its grace period survive the hide. When all active graphical sessions for a user are locked, shisad clears both states unless `clear_on_screen_lock` is disabled. It reacts to logind signals and checks `LockedHint` every 30 seconds as a fallback. Daemon restart clears tickets. Ticket expiry alone affects subsequent launches and leaves existing window allowance in place.

An instance records the PID and start time of the admitted process. Its descendants qualify while that process is alive. Grace-period expiry and screen locking leave instances intact. After a daemon restart the instance table is empty, but an already-running app can still execute its own protected image.

## Autostart

Protected autostart apps prompt by default. With `autostart = "allow"`, the daemon allows a matching `app-<id>@autostart.service` cgroup during the first 30 seconds of the session, using systemd escaping such as `\x2d` for hyphens. Settings explains that processes under the same account can imitate this unit name.

## Coverage limits

Already-open windows are handled by [window management](window-management.md). Launch gating covers executions of marked inodes. Copied binaries, direct runtime invocation, app data access, and Steam game executables without configured protected paths fall outside that coverage. Use separate system accounts to isolate application data.

## Verification

See [Tests and verification](../testing.md) for allow, deny, file replacement, and terminal entry-point checks. Live checks confirmed that execution from inside a Flatpak sandbox hits the host inode mark and that hard links and symlinks share coverage. See [Arch packaging](../packaging.md#installation-and-upgrade-verification) for the upgrade re-marking check.

The service sandbox was verified on 2026-11-15: the system bus, SIGHUP reload, and interception all work under `SystemCallFilter=@system-service fanotify_init fanotify_mark` with `SystemCallErrorNumber=EPERM`. On systemd 262.2, `systemd-analyze syscall-filter` lists `fanotify_init` and `fanotify_mark` in `@privileged` only, not in `@mount`, so the unit has to name them explicitly.
