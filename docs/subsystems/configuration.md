# Configuration and interfaces

## Configuration

Each user has a root-owned configuration file at `/etc/shisa/users/<username>.toml`.

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

`grace_seconds` is capped at 31,536,000 (one year) to keep expiry arithmetic bounded. `prompt_timeout_seconds` must be between 5 and 85. A protected app must have at least one path. `SetOption` rejects out-of-range values before prompting. When loading a file edited by root, shisad clamps the grace period and wait limit. Use `SetProtected` to remove protection entirely.

A reload is fail-closed. If the config directory or one of its entries cannot be read, a user's file cannot be read or parsed, or an account name cannot be resolved, shisad keeps the policy it already had instead of treating the user as unprotected. A missing file is the only case that means the user has no configuration yet.

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
Lock(app_id: s) -> ()                # empty ID locks all apps for the caller
Unlock(app_id: s) -> ()              # authorize an unlock for a protected app
RegisterAgent() -> ()                # subscribe to state notifications as the session agent
WindowStates() -> as                # apps with window allowance
RelockSeconds() -> a(su)             # focus-loss delays, omitting disabled apps
signal StateChanged(app_id: s, unlocked: b)
signal SessionUnlocked()
RecentLaunches() -> (a(txsisxx)ttt)
Diagnostics() -> (sxxxttsb)
```

`ListApps` returns both configured and last successfully marked paths. An unavailable path appears only in the configured list. Settings and `shisa status` identify it as inactive. `GlobalConfig` falls back to a 300-second grace period, clearing on screen lock, and a 60-second prompt timeout.

`SetProtected`, `SetOption`, and `Unlock` authorize the caller's unique name through polkit. Each settings change requires its own authorization even if a launch ticket exists. An empty app ID in `SetOption` selects a global option. Changing an app's grace period drops its current ticket and preserves its window allowance. The first successful settings change creates the user's file as root with mode 0644 when needed. `Lock` only revokes allowance and requires no authentication.

`StateChanged` is sent to the user's registered agent after settings changes, ticket issue, `Lock`, screen-lock revocation, consumption of a one-use ticket, or ticket expiry. `SessionUnlocked` signals the agent to refresh window handling on session unlock. `WindowStates` reports window allowance independently from launch tickets. `RelockSeconds` reports the configured focus-loss delays.

### Diagnostics

`RecentLaunches` returns the caller's most recent 64 events in chronological order and counts accumulated since daemon startup. Each event contains an ID, Unix timestamp in milliseconds, app ID, PID, outcome, wait deadline, and answer time. A zero deadline means there is no wait. A zero answer time means the request is waiting. The direct-allow outcomes are `ticket`, `instance`, `autostart`, and `selfexec`. A pending `asked` entry changes to `granted`, `timeout`, `cancelled`, `failed`, or `denied` when the check ends. See [Authentication](authentication.md#completion-and-cancellation) for outcome semantics. Restart clears the records and counts.

`Diagnostics` returns the version, start time, last reload, last incomplete mark operation, app count, mark count, config path, and password readiness. Times are Unix milliseconds and zero means the event has not occurred. A complete reload resets the mark-failure timestamp. Counts and the config path are for the caller, even if the config has yet to be created.

The password readiness flag checks only the `shisa-lock` password field in `/etc/shadow`. It reports ready if the file cannot be read. Settings uses the flag to show a setup hint. Both diagnostic methods are read-only and require no polkit prompt.

## Session D-Bus

The agent serves `io.github.RiverOnVenus.Shisa.Window` at `/io/github/RiverOnVenus/Shisa/Window`. `WindowEvent(app_id: s, pid: s, event: s) -> b` takes a decimal PID string and accepts `focused` and `unfocused` events for relock timers. The interface also provides `WindowState`, `HiddenApps`, `RestoreWindow`, `WindowsChecked`, `HiddenWindows`, and `VerifyLock`. See [Window management](window-management.md) for the acknowledgement and restoration contracts.

## CLI

```sh
shisa status
shisa lock [<app-id>]
shisa unlock <app-id>
shisa protect <app-id>
shisa unprotect <app-id>
shisa resolve <app-id>
```

The CLI and settings share the daemon's policy operations. The tray and CLI share the lock contract. The tray can show the measured window result, while the CLI reports daemon success and requests window verification through the session agent when available. For terminal authentication, `shisa unlock` can start `pkttyagent`. The `resolve` command is read-only and reports derived paths and coverage notes.

## Verification

See [Tests and verification](../testing.md) for configuration changes and D-Bus permission coverage.
