#!/usr/bin/env bash
# Run inside the disposable Debian build container. The source is mounted
# read-only at /src, and the four binaries are copied to /out/bin for nfpm.
set -euo pipefail

export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y --no-install-recommends \
    ca-certificates curl tar gzip build-essential pkg-config \
    dbus libdbus-1-dev \
    libwebkit2gtk-4.1-dev libgtk-3-dev libxdo-dev \
    librsvg2-dev libssl-dev

curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --profile minimal --default-toolchain stable
# shellcheck source=/dev/null
. "$HOME/.cargo/env"

# Build in the container's writable layer. The checkout's target directories
# and node_modules are not needed, and copying them would waste time and disk.
mkdir -p /build
tar -C /src \
    --exclude=./.git \
    --exclude=./target \
    --exclude=./gui/shisa-settings/src-tauri/target \
    --exclude=./gui/shisa-settings/node_modules \
    --exclude=./packaging/arch/src \
    --exclude=./out \
    -cf - . | tar -C /build -xf -
cd /build

cargo test --workspace --release --locked
( cd gui/shisa-settings/src-tauri && cargo test --release --locked )
cargo build --release --locked
( cd gui/shisa-settings/src-tauri && cargo build --release --locked --features custom-protocol )

install -Dm755 target/release/shisad target/release/shisa target/release/shisa-agent /out/bin/
install -Dm755 gui/shisa-settings/src-tauri/target/release/shisa-settings /out/bin/
# The container runs as root. Give the mounted output back to the runner so
# the workflow can clean it up and nfpm can write the package beside it.
chown -R "${HOST_UID:-0}:${HOST_GID:-0}" /out/bin
