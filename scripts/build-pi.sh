#!/usr/bin/env bash
# Cross-compile rultra for the Raspberry Pi 5 (aarch64) from an x86_64 host.
#
# Why `env -u RUSTFLAGS`: a RUSTFLAGS environment variable OVERRIDES the
# per-target `rustflags` in .cargo/config.toml rather than merging with it.
# A host-wide `-C link-arg=-fuse-ld=mold` therefore leaks into the cross link,
# where the aarch64 gcc cannot find that linker and fails with the misleading
# `collect2: fatal error: cannot find 'ld'`. Clearing it is the only fix that
# works regardless of how the host shell is configured.
#
# Build on the host, not on the Pi: cargo on an SD card starves the desktop
# for I/O (observed: load 11.7, 50% iowait, UI frozen).
set -euo pipefail
TARGET=aarch64-unknown-linux-gnu
cd "$(dirname "$0")/.."
env -u RUSTFLAGS cargo build --release --target "$TARGET" --features rultra-sense/hardware "$@"
echo "built: target/$TARGET/release/rultra-sense"
