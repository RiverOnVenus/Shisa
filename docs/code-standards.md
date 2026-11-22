# Code standards

## Code

- Follow the module's naming, types, and error handling. Read callers and existing tests before changing behavior. Preserve interface behavior outside the change's scope.
- Trace changes from their normal user entry point to the resulting state, side effects, and feedback. When callers share an operation, keep the shared contract consistent and check each affected caller. Document entry-point-specific feedback where it matters.
- Treat a failed read or parse as unknown, never as empty or as no change. When a configuration cannot be read or does not parse, keep the protection that was in force and report the failure. Never let a read error reduce what is protected.
- When a change alters a limit, default, timeout, or list, find every caller, saved copy, document, and test that assumes the old meaning and update them together. Check the other callers and branches of the same operation for the same defect. The same limit often exists in `shisa-core`, the daemon, the KWin script, and the settings app.
- Take the behavior of KWin, polkit, systemd, and Tauri from their own documentation, source, or a real probe, not from field names or guesses. When a behavior differs by version, record the version in the subsystem reference.
- Run `cargo fmt --all -- --check` for Rust changes. Preserve useful context on system call and D-Bus errors. Avoid `unwrap()` for expected failures on daemon request paths.

## Comments

- Write new and updated comments in English, including Rust documentation comments, frontend code, and scripts. Document caller requirements and important failure behavior on public interfaces.
- Explain constraints that are hard to infer from the code, such as why polkit cancellation must reuse the original bus connection. Update affected comments when behavior changes. Use a `// SAFETY:` comment to explain why each Rust `unsafe` block meets its safety requirements.

## Tests

- When existing coverage misses a bug, add a regression test. Check that it fails on the old behavior for the expected reason and passes with the fix.
- When changing defaults, config readers, or migrations, cover old configurations that omit the field and configurations with an explicit user value, including explicit values such as zero or false when supported.
- Use [Tests and verification](testing.md) to pick checks for the affected paths. Report automated, isolated-session, and live desktop results separately when applicable. Identify relevant checks that were skipped.

## Commits

- Write commit subjects and bodies in English. Prefix the subject with the area the commit changes: a crate name, `settings`, `packaging`, `docs`, or `tests`. Follow it with what the user now sees, for example `shisa-core: wait for KWin to acknowledge the new script before unloading the old one`. Use the body to explain the reason and effect when needed.
- Keep each commit independently reviewable and focused on one change. An unrelated fix found on the way goes in its own commit, so a revert takes back one change and leaves the rest. Include related tests and documentation. Check `git diff --cached` before committing.
- `main` requires the `rust` and `frontend` checks on the exact tip, administrators are included, and there is no pull-request-only rule. A locally made commit has no check runs yet, so `git push origin main` is refused with GH006. Without a pull request, push the commit to a temporary branch, wait for the Test workflow on that commit, then fast-forward `main` to it.

## Documentation

- Write developer documentation in English. Keep the root [README](../README.md) and its [Simplified Chinese version](../README.zh-CN.md) aligned when user-facing behavior changes.
- Update the responsible subsystem reference in the same change when its interface, state transitions, user-visible behavior, compatibility, or failure handling changes. Put cross-component guarantees in the [architecture overview](architecture.md) and link to them from related pages.
- Link important behavior to source files, named symbols, and existing tests. Prefer stable file links with symbol names in the text. Keep README diagrams compatible with Mermaid 9.
