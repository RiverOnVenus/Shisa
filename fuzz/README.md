# Fuzz targets

These targets look for panics in the input parsers. They need a nightly
toolchain and `cargo-fuzz`:

```sh
rustup toolchain install nightly --profile minimal
cargo install cargo-fuzz --version 0.13.2 --locked
```

CI uses the same cargo-fuzz version. A newer libFuzzer may report a different
crash, so reproduce an uploaded artifact with this one.

Run one target for a bounded time:

```sh
cargo +nightly fuzz run config_parse -- -max_total_time=60
cargo +nightly fuzz run exec_split -- -max_total_time=60
cargo +nightly fuzz run fanotify_event -- -max_total_time=60
```

| Target           | Input                                         |
| ---------------- | --------------------------------------------- |
| `config_parse`   | `/etc/shisa/users/<user>.toml`                |
| `exec_split`     | the `Exec=` word splitting of a desktop entry |
| `fanotify_event` | one `FAN_OPEN_EXEC_PERM` record buffer        |

A crash is written to `fuzz/artifacts/<target>/`. Reproduce it with
`cargo +nightly fuzz run <target> fuzz/artifacts/<target>/<file>`.

The crate has its own workspace, so the root `cargo test` and the package
build do not compile it. CI runs every target weekly and on demand
(`.github/workflows/fuzz.yml`).
