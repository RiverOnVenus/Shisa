# Tests and verification

## Rust and the settings app

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets
(cd gui/shisa-settings && npm ci && npm run build)
(cd gui/shisa-settings/src-tauri && cargo build --features custom-protocol)
```

## Daemon regression tests

After changing `shisad`, also run `tests/gate.sh`, `tests/auth.sh`, and `tests/settings.sh`. All three require sudo, so obtain approval before running them. Use a time limit for the test daemon, use `tests/fake-polkit`, and mark only temporary files or KWrite. Keep the user's open browser, terminal, and shell outside the test and keep real passwords out of scripts. Report which fanotify and polkit paths the tests exercised.

## Window and authentication verification

1. In an isolated KWin session, open a protected window and an ordinary window. Lock all apps and verify that the protected window disappears and the ordinary window stays usable. Repeat the operation and check that the acknowledgement reflects the current state.
2. Test the no-match case and the unconfirmed result when the agent or KWin does not respond. Check that refocusing cancels the focus-loss timer and that timer expiry hides the window.
3. For live desktop checks, follow the [Arch packaging guide](packaging.md). Verify tray interaction, authentication cancellation, restoration of the original window and unsaved content, foreground activation, taskbar, and Overview as relevant. Real Wayland window clicks require manual input because xdotool cannot inject them.
4. When checking another settings-app build, close the window, clear its WebKit cache at `$XDG_RUNTIME_DIR/shisa-settings`, and reopen it.
