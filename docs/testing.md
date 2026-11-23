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

The owner-check tests in `crates/shisad/tests/config_owner.rs` cannot create root-owned files as the normal user, so each re-executes itself under `unshare --user --map-root-user`. They print a skip when user namespaces are unavailable instead of passing a different check. `shisa-core`'s `files_read_for_an_account_must_be_its_own` skips when the test process runs as root, because root can read the file the test expects to be refused. The Fedora and Debian package jobs run their tests as root in the build containers.

## Distribution packages

The [packages workflow](../.github/workflows/packages.yml) builds the Arch, Fedora, and Debian packages on a published release and on manual dispatch. The workflow runs the Rust workspace tests, the settings frontend tests, the Tauri tests, and the KWin script tests across its jobs: the frontend job runs the settings and KWin script suites, the Fedora and Debian jobs run the Rust workspace and Tauri suites, and the Arch job's `makepkg` check covers all four. The Fedora and Debian jobs build the binaries in `fedora:latest` and `debian:12` and package them with nfpm.

The Fedora and Debian packages still need a live KDE check and, on Fedora, an SELinux check before they are treated as verified. See [Distribution-specific verification](packaging.md#distribution-specific-verification).

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

After changing `shisad`, also run `tests/gate.sh`, `tests/auth.sh`, and `tests/settings.sh`. All three require sudo, so obtain approval before running them. Use a time limit for the test daemon, use `tests/fake-polkit`, and mark only temporary files. `gate.sh` and `settings.sh` also mark `/usr/bin/kwrite`, and only `gate.sh` launches it. `auth.sh` uses temporary files only. Keep the user's open browser, terminal, and shell outside the test and keep real passwords out of scripts. Report which fanotify and polkit paths the tests exercised.

On 2026-11-22, the working tree with the 0600/0700 configuration modes and the desktop scan budget passed `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked`, `cargo test --workspace --locked` (167 tests), `node --test crates/shisa-agent/tests/*.test.mjs` (12), `npm test` (9 Node and 7 vitest tests), the settings Tauri tests (3), `bash -n` on the test scripts, and `systemd-analyze verify data/shisad.service`. `tests/gate.sh`, `tests/auth.sh`, and `tests/settings.sh` each reported 0 failed. The fanotify checks covered deny and allow, symlinks and hard links, ticket-covered launches, instances, the zero-grace one-shot ticket, file replacement with SIGHUP and the 30-second re-mark, a broken and a foreign-owned config, the Plasma `kioclient` path with KWrite, and daemon shutdown. The polkit checks used `tests/fake-polkit`, not the real desktop agent or PAM, and covered refusal, one check per app group, the 5-second wait limit and cancellation, and a ticket not approving a settings change. `settings.sh` asserted the 0600 file and 0700 directory modes on its temporary config directory. The scripts force `LC_ALL=C` and a temporary `XDG_CONFIG_HOME`, because their assertions are English and a saved `zh-CN` choice would otherwise translate the CLI output.

On 2026-11-22, `cargo test --workspace` (169 tests) passed after the account-name validation. The unidentifiable-exec denial has no direct test seam.

On 2026-11-22, after the last two shisad hardening commits, `cargo build --release --locked` rebuilt the binaries and `tests/gate.sh`, `tests/auth.sh`, and `tests/settings.sh` each reported 0 failed. `gate.sh` included the Plasma `kioclient` path with KWrite and the 30-second re-mark. `settings.sh` asserted the 0600 file and 0700 directory modes.

On 2026-11-22, the small hardening branch passed `cargo test --workspace --locked` (169 tests), the settings Tauri tests (4), `node --test crates/shisa-agent/tests/*.test.mjs` (12), `npm test` (9 Node and 7 vitest tests), `cargo clippy`, and the format checks. `tests/gate.sh`, `tests/auth.sh`, and `tests/settings.sh` each reported 0 failed, including the Plasma `kioclient` path with KWrite and the 0600/0700 modes.

On 2026-11-22, the user verified the live Wayland session after installing the test package: the settings list and icons loaded, Lock all hid the protected windows while an ordinary window stayed usable, the restore panel returned a window without a prompt inside the grace period, and the app-tray path behaved the same. The config modes were 0700/0600.

On 2026-11-22, `cargo test --workspace --locked` (171 tests) passed after the session environment and file ownership constraints. The manager-environment test skips when no user manager is available.

On 2026-11-22, `cargo test --workspace --locked` (172 tests) covered the configuration failure timestamp and the load failure flag. The settings diagnostics card renders the new field.

On 2026-11-22, after the session environment and configuration-failure changes, `tests/gate.sh`, `tests/auth.sh`, and `tests/settings.sh` each reported 0 failed, including the exact manager resolution and the nine-field `Diagnostics` tuple. The package check passed 172 workspace tests, 4 settings Tauri tests, and the frontend and KWin script suites. The user verified the live settings checks.

On 2026-11-22, the settings window's Wayland shadow was checked. In `gui/shisa-settings/src-tauri`, `cargo fmt --all -- --check`, `cargo test --features custom-protocol` (4 tests) and `cargo build --features custom-protocol` passed. A standalone GTK 3 probe reproduced tao's frameless setup (an empty titlebar with `decorated` off) with no shadow, and showed the theme's client-side decoration shadow once `set_decorated(true)` followed the show. The built settings window, run from `target/debug` in the live KDE Plasma 6 Wayland session, then showed that shadow around an otherwise unchanged frameless page. Dragging and edge resizing were not exercised here and still need a check in the live session.

On 2026-11-22, the same window's top corners looked rounded while the bottom corners looked square. Pixel inspection of the live capture found the page square at all four corners. The GTK theme in this session reports Adwaita, whose `decoration` rule rounds only the top corners (`border-radius: 8px 8px 0 0`), and its shadow follows that shape. The main window now carries a class whose CSS provider sets `decoration { border-radius: 0; }`. In a new capture the corner alpha changed at the top and stayed byte-identical at the bottom, so the shadow source is square at all four corners while keeping the theme's blur and resize margin. The restore window now uses the same class and GTK decoration shadow. The theme's downward shadow offset still makes the top edge lighter than the bottom.

## Window and authentication verification

1. In an isolated KWin session, open a protected window and an ordinary window. Lock all apps and verify that the protected window disappears and the ordinary window stays usable. Repeat the operation and check that the acknowledgement reflects the current state.
2. Test the no-match case and the unconfirmed result when the agent or KWin does not respond. On a desktop without KWin, check that the lock result says there is no window layer instead of calling the state unconfirmed. Check that refocusing cancels the focus-loss timer and that timer expiry hides the window. Check that the tray's Shisa eyes turn amber while the hidden app's ticket is valid, that its tooltip names how many hidden apps can restore without a prompt, that Lock all or ticket expiry turns them blue, and that restoring the window returns them to green. Stopping shisad should turn them red. Restore it within the app's grace period from the Shisa restore panel and from the app's own tray icon, and check that neither path asks for a password and that the window never appears before the ticket check finishes. Then let the ticket expire or run a manual lock and check that the restore panel asks for authentication while the app's own tray activation leaves the window hidden.
3. For live desktop checks, follow the [Arch packaging guide](packaging.md). Verify tray interaction, authentication cancellation, restoration of the original window and unsaved content, foreground activation, taskbar, and Overview as relevant. Real Wayland window clicks require manual input because xdotool cannot inject them.
4. When checking another settings-app build, close the window, clear its WebKit cache at `$XDG_RUNTIME_DIR/shisa-settings`, and reopen it.
5. After replacing a tray icon under the same name, restart `plasma-plasmashell.service` so Plasma drops its cached pixmaps. The agent does not need a restart unless its binary changed.
