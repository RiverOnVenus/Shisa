# Configuration and interfaces

## Configuration

Each user has a root-owned configuration file at `/etc/shisa/users/<username>.toml`. The account name must be a single path component. A name that is empty, `.`, `..`, or contains a slash is refused instead of being joined into the path.

```toml
[global]
grace_seconds = 300
clear_on_screen_lock = true
prompt_timeout_seconds = 60

[[apps]]
id = "firefox-nightly"
paths = ["/usr/lib/firefox-nightly/firefox"]
grace_seconds = 600
autostart = "prompt"
relock_after_unfocus_seconds = 0   # 0 disables hiding, 1 means immediate
```

The [launch gate resolver](launch-gate.md#protected-path-resolution) fills in `paths`. App IDs omit the `.desktop` suffix in the config, D-Bus API, and CLI.

`grace_seconds` is capped at 31,536,000 (one year) to keep expiry arithmetic bounded. `prompt_timeout_seconds` must be between 5 and 85. A protected app must have at least one path and at most 64. `SetOption` rejects out-of-range values before prompting. When loading a file edited by root, shisad clamps the grace period and wait limit. Use `SetProtected` to remove protection entirely.

A reload is fail-closed. If the config directory or one of its entries cannot be read, a user's file cannot be read or parsed, or an account name cannot be resolved, shisad keeps the policy it already had instead of treating the user as unprotected. The diagnostics record the time of the last such failure. That failure also keeps a retry scheduled: the next 30-second revalidation tries again, and repeated failures back off to half an hour. Repairing the file restores protection without a signal. A missing file is the only case that means the user has no configuration yet.

A settings write is fail-closed too. shisad refuses a config directory that is not root's or that group or others can write, removes a stale temp file, and creates the new one without following a symlink. The directory is kept at 0700, the result is written as 0600, and it is renamed into place. The package's install script tightens the directory and files of an earlier installation, because the files list the apps and paths each user protects.

### Interface language

The settings window, restore window, tray menu and notifications, and `shisa` commands share one per-user choice. It lives in a user-writable file, apart from the root-owned policy file, because changing it must not need polkit authorization:

```toml
# $XDG_CONFIG_HOME/shisa/ui.toml, usually ~/.config/shisa/ui.toml
language = "zh-CN"   # or "en"
```

Without a saved choice, the programs read `LC_ALL`, then `LC_MESSAGES`, then `LANG`. A `zh` locale selects Simplified Chinese and every other locale English. A missing, unreadable or malformed file falls back to the locale rather than failing. The daemon reads this user-writable file while it represents the account, so the read accepts only a small regular file, opens it without blocking on a FIFO, caps it at 64 KiB, and requires the opened file to belong to the account.

The settings window writes the file from the General page's language selector. The tray re-reads it every two seconds, so its menu, tooltip and notifications follow a change without a restart. The restore window reads it when it opens. Desktop files' `Name[locale]` still follow the session locale: the choice changes Shisa's own text, not application names.

The daemon keeps D-Bus error strings in English so the bus contract and the debug log stay stable. The settings shell and the CLI translate the known ones for the Chinese interface (`shisa_core::i18n::error_text`). Resolution notes from `ResolveApp` are formatted in the caller's saved language because they are content rather than error codes. The `.policy` file carries both languages through `xml:lang`, and polkit selects the action text by the session locale. The detail that names the target of a global settings change uses the saved choice instead.

## System D-Bus

The service is `io.github.RiverOnVenus.Shisa`, at `/io/github/RiverOnVenus/Shisa/Manager`. It derives the caller's identity from bus connection credentials and scopes state and settings to that user.

```text
ListApps() -> a(sbasas)              # app ID, ticket present, configured paths, marked paths
ListInstalled() -> a(sss)            # app ID, display name, icon name
AppConfig(app_id: s) -> (tbbt)        # grace period, allow autostart, protected, focus-loss delay
GlobalConfig() -> (tbt)              # grace period, clear on screen lock, prompt timeout
ResolveApp(app_id: s) -> (asas)      # derived paths and coverage notes, read-only
SetProtected(app_id: s, on: b) -> ()
SetOption(app_id: s, key: s, value: v) -> ()
Lock(app_id: s) -> ()                # empty ID drops every ticket and window permission
Hide(app_id: s) -> ()                # hide one app's windows and keep its ticket
Unhide(app_id: s) -> b               # show them again while the ticket is valid
Unlock(app_id: s) -> ()              # authorize an unlock for a protected app
RegisterAgent() -> ()                # subscribe to state notifications as the session agent
RegisterSettings() -> ()             # register the settings window for the lock signal
WindowStates() -> as                # apps with window allowance
RelockSeconds() -> a(su)             # focus-loss delays, omitting disabled apps
signal StateChanged(app_id: s, unlocked: b)
signal SessionUnlocked()
signal SettingsLocked()
RecentLaunches() -> (a(txsisxx)ttt)
Diagnostics() -> (sxxxttsbx)
```

`ListApps` returns both configured and last successfully marked paths. An unavailable path appears only in the configured list. Settings and `shisa status` identify it as inactive. `GlobalConfig` falls back to a 300-second grace period, clearing on screen lock, and a 60-second prompt timeout.

`SetProtected`, `SetOption`, and `Unlock` authorize the caller's unique name through polkit. Each settings change is checked even if a launch ticket exists, but polkit's retained `auth_admin_keep` decision can answer a later change from the same window without a prompt. The settings window calls `RegisterSettings` and keeps one system-bus connection, so a global lock or a screen lock can send it `SettingsLocked` and end that decision by exiting (see [Authentication](authentication.md#policy-and-identity)). An empty app ID in `SetOption` selects a global option. Changing an app's grace period drops its current ticket and preserves its window allowance. The first successful settings change creates the user's file as root with mode 0600 when needed, under the root-only directory. `Lock`, `Hide`, and `Unhide` require no authentication.

`Lock` drops both the ticket and the window permission. `Hide` drops only the window permission, so the focus-loss timer conceals a window without ending its grace period. `Unhide` restores the permission and returns true only while the app still holds a valid ticket. A false result means the caller has to authenticate through `Unlock` first. A manual lock always clears the ticket, and a screen lock clears it when `clear_on_screen_lock` is enabled, so both defeat `Unhide`. Among `Lock`, `Hide`, and `Unhide`, only `Lock` accepts an empty app ID.

`StateChanged` is sent to the user's registered agent after settings changes, ticket issue, `Lock`, `Hide`, a ticket- or autostart-covered launch that restores window allowance, screen-lock revocation, consumption of a one-use ticket, or ticket expiry. `Unhide` does not send it: the caller applies the restored window state itself or asks the session agent to do so. `SessionUnlocked` signals the agent to refresh window handling on session unlock. `SettingsLocked` is sent to registered settings windows after a global `Lock` or when every graphical session of the user locks. It asks them to exit so polkit's process-keyed settings authorization cannot outlive the lock. `WindowStates` reports window allowance independently from launch tickets. `RelockSeconds` reports the configured focus-loss delays.

### Diagnostics

`RecentLaunches` returns the caller's most recent 64 events in chronological order and counts accumulated since daemon startup. Each event contains an ID, Unix timestamp in milliseconds, app ID, PID, outcome, wait deadline, and answer time. A zero deadline means there is no wait. A zero answer time means the request is waiting. The direct-allow outcomes are `ticket`, `instance`, `autostart`, and `selfexec`. A pending `asked` entry changes to `granted`, `timeout`, `cancelled`, `failed`, `revoked`, or `denied` when the check ends. `revoked` means a lock started after the request and the held launch was denied. See [Authentication](authentication.md#completion-and-cancellation) for outcome semantics. Restart clears the records and counts.

`Diagnostics` returns the version, start time, last reload, last incomplete mark operation, app count, mark count, config path, password readiness, and the last configuration read or parse failure. Times are Unix milliseconds and zero means the event has not occurred. A complete reload resets the mark-failure timestamp, and a load that reads every configuration file resets the configuration-failure timestamp. Counts and the config path are for the caller, even if the config has yet to be created.

The password readiness flag reads `/etc/shadow` and returns only whether the `shisa-lock` entry is set. The file's contents never reach the caller. It reports ready if the file cannot be read. Settings uses the flag to show a setup hint, and the hint's Check again button re-reads diagnostics so setting the password does not require reopening the window. Both diagnostic methods are read-only and require no polkit prompt.

Settings compares the daemon's reported version with its own and shows a notice when they differ, because an upgraded package keeps the old shisad running until its service restarts. The sidebar also checks the session bus for `io.github.RiverOnVenus.Shisa.Window`, the name `shisa-agent` serves. It shows one health line that covers the services and the setup problems that make authentication fail: healthy when both answer and authentication can work, a warning for a failing service, a missing app-lock password, or a version mismatch, and a failure when both services are down. The tooltip carries the two-service breakdown, the remedy, and the password or version hint. A missing agent is not a daemon error: the daemon can be healthy while the tray and window layer are unavailable. The General page keeps the recent-launch entry and the daemon's routine numbers in one diagnostics card. The marking result and any configuration failure stay in the card's header row, because the health line does not carry those results. The version, uptime and config path sit behind a collapsed Details row, so the card does not show routine numbers unless someone is diagnosing a failure. The password and version notices appear on the General page only. On every other page the health line carries the warning.

## Session D-Bus

The agent serves `io.github.RiverOnVenus.Shisa.Window` at `/io/github/RiverOnVenus/Shisa/Window`. `WindowEvent(app_id: s, pid: s, event: s) -> b` takes a decimal PID string and accepts `focused` and `unfocused` events for relock timers, and `closed` when a protected app loses its last window. A `closed` event makes the agent call `Hide`, which drops the app's window permission and keeps its ticket. The pid is empty for `closed`. The interface also provides `WindowState`, `HiddenApps`, `RestoreWindow`, `RestoreIfTicketed`, `WindowsChecked`, `HiddenWindows`, `VerifyLock`, `VerifyLockReport`, and `SaveWindowSnapshots`. `VerifyLockReport` returns one call's result: a status that is confirmed, partial, unconfirmed, or no window layer, the script generation, the matched and concealed counts, and the app ids whose windows stayed visible. `SaveWindowSnapshots(generation: s, sequence: s, snapshots: s) -> b` stores the window-original journal before a hide. Only the current script generation and a newer sequence may replace it, and the text is capped at 1 MiB. `WindowsChecked` carries the generation, the matched and concealed totals, and the apps that still have a visible window. `WindowEvent`, `WindowsChecked`, `HiddenWindows`, `RestoreIfTicketed`, and `SaveWindowSnapshots` are accepted only from the current owner of `org.kde.KWin`. `RestoreWindow` is the restore panel's path and may authenticate. `RestoreIfTicketed` is the KWin script's path for an app's own tray activation: it restores the window only while a ticket is valid and never starts polkit. See [Window management](window-management.md) for the acknowledgement and restoration contracts.

## CLI

```sh
shisa --help
shisa status
shisa lock [<app-id>]
shisa unlock <app-id>
shisa protect <app-id>
shisa unprotect <app-id>
shisa resolve <app-id>
```

The CLI and settings share the daemon's policy operations. The tray and CLI share the lock contract. The tray can show the measured window result, while the CLI reports daemon success and prints the window result the session agent reports when available. For terminal authentication, `shisa unlock` can start `pkttyagent`. The `resolve` command is read-only and reports derived paths and coverage notes. `--help` and `-h` print the command list and the app-id format to stdout and exit 0. An unknown command prints the same list to stderr and exits 2. `status` names each protected app as authorized or locked, marks paths that are not in effect, and points to `shisa protect` or the settings window when the user has no protected apps. `protect` reads the app back with `ListApps` after the daemon saves the change and names configured paths that are not marked. It exits 1 when no path is in effect and 0 with a warning when only some are. Output follows the interface language above. The public text says authorization, while tickets remain the internal term for the same time-limited permission.

## Verification

See [Tests and verification](../testing.md) for configuration changes and D-Bus permission coverage.

On 2026-11-21, the interface language was checked with `cargo test --workspace` and `cargo clippy --workspace --all-targets` (both clean), `(cd gui/shisa-settings && npm test)` (9 passed) and `npm run build`, plus the settings shell's `cargo test` (3 passed) and `cargo build --features custom-protocol`. The live selector click and the tray language change were not exercised here and still need a check in a Wayland session.

On 2026-11-24, the settings window's descriptions were shortened and the mechanics moved into per-card `Notes` folds. The dictionaries keep the same key set in both languages (156 keys each), and `npm test` (9 node tests, 12 vitest tests) and `npm run build` pass. The fold is a `<details>` element so the page search still reads the text, and the search still opens a matching fold. The app and General pages were checked in a live Wayland session, and `docs/images/settings.png` and `settings-zh.png` were regenerated from the installed build at 1175x775.
