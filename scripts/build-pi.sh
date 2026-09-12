#!/usr/bin/env bash
# Cross-compile rultra for the Raspberry Pi 5 (aarch64) from an x86_64 host.
#
# Builds inside a Debian bookworm container by default. This is not fussiness:
# Raspberry Pi OS bookworm ships glibc 2.36, while a current Ubuntu host has
# 2.39. A host-toolchain build links against the newer glibc and dies on the Pi
# with
#
#     /lib/aarch64-linux-gnu/libc.so.6: version `GLIBC_2.39' not found
#
# ...but only for binaries that actually reference a newer symbol. Small
# binaries link fine and give a false sense that the host build is portable;
# adding one dependency (tokio, in our case) breaks it. Matching the target's
# glibc is the only thing that makes the result reliable.
#
# RUSTFLAGS is cleared because an environment RUSTFLAGS OVERRIDES the per-target
# rustflags in .cargo/config.toml rather than merging with them, so a host-wide
# `-C link-arg=-fuse-ld=mold` leaks into the cross link and fails as the
# misleading `collect2: fatal error: cannot find 'ld'`.
#
# Set RULTRA_BUILD_NATIVE=1 to build with the host toolchain instead (faster,
# but only safe for binaries you have verified run on the target).
set -euo pipefail
TARGET=aarch64-unknown-linux-gnu
cd "$(dirname "$0")/.."

if [ "${RULTRA_BUILD_NATIVE:-0}" = "1" ] || ! command -v docker >/dev/null 2>&1; then
  echo "building with the host toolchain (glibc mismatch is possible)"
  env -u RUSTFLAGS cargo build --release --target "$TARGET" "$@"
  echo "built: target/$TARGET/release/"
  exit 0
fi

docker run --rm -v "$PWD":/src -w /src \
  -e CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
  -e CARGO_TARGET_DIR=/src/target-bookworm \
  -e RUSTFLAGS= \
  rust:1-bookworm sh -c "
    set -e
    if ! command -v aarch64-linux-gnu-gcc >/dev/null; then
      apt-get update -qq >/dev/null && apt-get install -y -qq gcc-aarch64-linux-gnu >/dev/null
    fi
    rustup target add $TARGET >/dev/null 2>&1 || true
    cargo build --release --target $TARGET $*
  "
echo "built: target-bookworm/$TARGET/release/"
