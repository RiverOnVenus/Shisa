# Code standards

## Code

- Follow the module's naming, types, and error handling. Read callers and existing tests before changing behavior. Preserve interface behavior outside the change's scope.
- Trace changes from their normal user entry point to the resulting state, side effects, and feedback. When callers share an operation, keep the shared contract consistent and check each affected caller. Document entry-point-specific feedback where it matters.
- Run `cargo fmt --all -- --check` for Rust changes. Preserve useful context on system call and D-Bus errors. Avoid `unwrap()` for expected failures on daemon request paths.

## Comments

- Write new and updated comments in English, including Rust documentation comments, frontend code, and scripts. Document caller requirements and important failure behavior on public interfaces.
- Explain constraints that are hard to infer from the code, such as why polkit cancellation must reuse the original bus connection. Update affected comments when behavior changes. Use a `// SAFETY:` comment to explain why each Rust `unsafe` block meets its safety requirements.

## Tests

- When existing coverage misses a bug, add a regression test. Check that it fails on the old behavior for the expected reason and passes with the fix.
- When changing defaults, config readers, or migrations, cover old configurations that omit the field and configurations with an explicit user value, including explicit values such as zero or false when supported.
- Use [Tests and verification](testing.md) to pick checks for the affected paths. Report automated, isolated-session, and live desktop results separately when applicable. Identify relevant checks that were skipped.

## Commits

- Write commit subjects and bodies in English. Use an imperative subject that names the change, for example `Wait for KWin to acknowledge the new script before unloading the old one`. Use the body to explain the reason and effect when needed.
- Keep each commit independently reviewable and focused on one change. Include related tests and documentation. Check `git diff --cached` before committing.

## Documentation

- Write developer documentation in English. Keep the root [README](../README.md) and its [Simplified Chinese version](../README.zh-CN.md) aligned when user-facing behavior changes.
- Update the responsible subsystem reference in the same change when its interface, state transitions, user-visible behavior, compatibility, or failure handling changes. Put cross-component guarantees in the [architecture overview](architecture.md) and link to them from related pages.
- Link important behavior to source files, named symbols, and existing tests. Prefer stable file links with symbol names in the text. Keep README diagrams compatible with Mermaid 9.
