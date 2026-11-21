# Tests and verification

- Write down a result only after the run that produced it has ended. Say what was run and what it printed. List each check that was not run as not run, and name each failure instead of calling it unrelated to the change.
- A test stops everything it starts. Stop or join every daemon, thread, and timer before the test returns. Run daemon tests on the private bus with `tests/fake-polkit`, and leave the user's configuration, `/etc`, and `/run` untouched.
- Leave a timing test room on both sides. Do not bound a test by a wall-clock duration a loaded machine can exceed, and keep fixtures far from the timeout they exercise. Reproduce a timing failure by injecting the delay, not by rerunning until it fails.

## Rust and the settings app

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets
(cd gui/shisa-settings && npm ci && npm run build)
(cd gui/shisa-settings && npm test)
(cd gui/shisa-settings/src-tauri && cargo test)
(node --test crates/shisa-agent/tests/*.test.mjs)
(cd gui/shisa-settings/src-tauri && cargo build --features custom-protocol)
```

The settings crate is outside the workspace, so `cargo test --workspace` does not reach it. Its frontend rules are plain JavaScript tested with Node's built-in runner, its component tests mount the real window under jsdom with vitest, and the Tauri shell has its own tests. `npm test` runs both frontend suites. The Arch package's `check()` runs every test suite above.

CI runs the formatting check, clippy, the workspace, settings window and KWin script suites on every push and pull request (`.github/workflows/test.yml`). The Tauri suite needs the WebKitGTK build stack, so it stays in the Arch package check rather than the push workflow.

The owner-check tests in `crates/shisad/tests/config_owner.rs` cannot create root-owned files as the normal user, so each re-executes itself under `unshare --user --map-root-user`. They print a skip when user namespaces are unavailable instead of passing a different check.

## Fuzzing

The fuzz targets cover the config file parser, the desktop `Exec` splitter and the fanotify record decoder. They need a nightly toolchain and `cargo-fuzz`, and run for a bounded time, so they are not part of `cargo test`:

```sh
cargo +nightly fuzz run config_parse -- -max_total_time=60
cargo +nightly fuzz run exec_split -- -max_total_time=60
cargo +nightly fuzz run fanotify_event -- -max_total_time=60
```

CI runs all three weekly and on demand (`.github/workflows/fuzz.yml`). See [fuzz/README.md](../fuzz/README.md) for the inputs and how to reproduce an artifact.

`shisa-agent`'s test `the_manager_supplies_wayland_display_when_the_agent_lacks_it` needs a live systemd user manager that exports `WAYLAND_DISPLAY`. It skips itself in containers and CI, which have no user manager, so verify that path in a desktop session with `env -u WAYLAND_DISPLAY -u DISPLAY cargo test -p shisa-agent manager`.

## Daemon regression tests

After changing `shisad`, also run `tests/gate.sh`, `tests/auth.sh`, and `tests/settings.sh`. All three require sudo, so obtain approval before running them. Use a time limit for the test daemon, use `tests/fake-polkit`, and mark only temporary files or KWrite. Keep the user's open browser, terminal, and shell outside the test and keep real passwords out of scripts. Report which fanotify and polkit paths the tests exercised.

## Window and authentication verification

1. In an isolated KWin session, open a protected window and an ordinary window. Lock all apps and verify that the protected window disappears and the ordinary window stays usable. Repeat the operation and check that the acknowledgement reflects the current state.
2. Test the no-match case and the unconfirmed result when the agent or KWin does not respond. Check that refocusing cancels the focus-loss timer and that timer expiry hides the window.
3. For live desktop checks, follow the [Arch packaging guide](packaging.md). Verify tray interaction, authentication cancellation, restoration of the original window and unsaved content, foreground activation, taskbar, and Overview as relevant. Real Wayland window clicks require manual input because xdotool cannot inject them.
4. When checking another settings-app build, close the window, clear its WebKit cache at `$XDG_RUNTIME_DIR/shisa-settings`, and reopen it.
