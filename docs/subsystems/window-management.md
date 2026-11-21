# Window management

## Hiding and restoring windows

The session agent and its KWin script manage windows that are already open. The script matches protected apps by `desktopFileName` first and `resourceClass` second. It minimizes locked windows, sets their opacity to zero, and removes them from the taskbar, switcher, and pager. Restoration affects windows Shisa hid and preserves ordinary user minimization. When the last app is unprotected, the agent restores its windows before unloading the script.

The agent builds versioned window state from `ListApps` and `WindowStates`. The script reads it over the session bus every 250 milliseconds and applies visibility and activation changes in place. Initial startup or loss of contact can trigger a fresh `loadScript` and `run`. The agent uses separate session-bus connections for KWin calls and acknowledgements, assigns a unique plugin name, and waits for the new script's acknowledgement before unloading the old one.

Window allowance is separate from a launch ticket. Ticket expiry affects the next launch. Manual `Lock` and the configured screen-lock policy revoke both states. A successful authenticated launch, `shisa unlock`, or allowed autostart sets window allowance. A daemon restart clears it, and an agent restart reads it back from the daemon.

The script reports focus changes. The agent times focus loss using `RelockSeconds`, cancels the timer on refocus or lock, and checks focus again before calling `Lock`. `StateChanged` triggers synchronization. The setting defaults to off. A value of 0 disables it and 1 means immediate hiding. Screen-lock handling is controlled by `clear_on_screen_lock` and applies when all active graphical sessions for that user are locked.

The window layer runs as the user. Processes under the same account can interfere with the script or window properties. The tray omits a quit action that could reveal locked windows, and idle apps remain running to preserve unsaved work.

## Lock all and its result

The tray and bare `shisa lock` share the system-bus `Lock("")` contract. Success from the daemon means it revoked launch tickets and window allowance. The tray requests a fresh window check, including on repeated lock operations. The CLI requests the same check when a session bus is available. A script load acknowledgement establishes that a script ran. The window acknowledgement reports how many windows matched and how many were actually hidden. A missing or incomplete result is reported as unconfirmed.

Left-clicking the tray opens the restore panel. Right-clicking offers Lock all and settings. The tray displays the last lock result in its tooltip, and the CLI can ask the agent to verify the same operation. After polkit authentication, the resident script restores the original window and attempts to activate it. Before opening settings, the agent fills missing display variables from the systemd user manager's `Environment`.

## Implementation and verification

| Step                                    | Source and symbols                                                                                                                                                                                             |
| --------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Start from the tray or CLI              | [Session agent](../../crates/shisa-agent/src/main.rs), `action_loop`. [CLI](../../crates/shisa-cli/src/main.rs), `lock` and its session-bus `VerifyLock` call.                                                 |
| Revoke daemon state                     | [System-bus handler](../../crates/shisad/src/bus.rs), `ManagerIface::lock`. [Policy](../../crates/shisad/src/policy.rs), `State::clear_tickets` and `State::lock_app`.                                         |
| Apply and acknowledge a window revision | [Window agent](../../crates/shisa-agent/src/window.rs), `Gate::sync`, `Gate::load`, and `windows_checked`.                                                                                                     |
| Run the KWin script                     | [Gate script template](../../crates/shisa-agent/src/gate-script.js), filled by `script_for` and written per generation.                                                                                        |
| Report the result                       | [Session agent](../../crates/shisa-agent/src/main.rs), `verify_lock` and `lock_summary`. The test `lock_feedback_requires_all_matched_windows_to_be_concealed` checks that partial concealment is unconfirmed. |

The script runs inside KWin, so its behavior is tested against a fake workspace in [gate_script.test.mjs](../../crates/shisa-agent/tests/gate_script.test.mjs). That covers matching by `desktopFileName` and `resourceClass`, concealment across the taskbar, switcher and pager, restoring a window an earlier generation hid, leaving a user-minimized window alone, focus reporting and state refreshes. Real KWin behavior still needs the live checks below.

See [Tests and verification](../testing.md#window-and-authentication-verification) for coverage of real windows, missing acknowledgements, and focus-loss relocking.

On 2026-11-21, 64Gram passed live Plasma Wayland verification on KWin 6.8.3 with Shisa 0.1.0-14. Lock all hid it from the desktop, taskbar, switcher, and Overview while an ordinary window remained usable. Cancelling authentication kept it hidden. Successful authentication returned it to the foreground with its conversation and draft intact. Repeating the operation gave the same result.

KWin reported the same `internalId` before locking, while hidden, and after restoration. Multiple lock operations received window acknowledgements, and KWin still reported the pre-existing plugin loaded, supporting reuse of the resident script during the test.
