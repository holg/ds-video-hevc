#!/usr/bin/env bash
# cross_build_on_mac.sh — build static Linux (musl) binaries for Synology NAS
# (works on macOS and Linux; CI uses it too).
# Usage: ./cross_build_on_mac.sh [target-triple ...]   (default: all Synology targets)
#
# The crate is pure Rust (no OpenSSL, no C deps), so the musl targets link with
# the rust-lld that ships with rustup — no external cross toolchain needed.

set -euo pipefail

BIN="dsvideo-passthrough"
ALL_TARGETS=(
  x86_64-unknown-linux-musl        # Intel/AMD models (e.g. DS1813+, cedarview)
  aarch64-unknown-linux-musl       # ARMv8 models (rtd1296, rtd1619b, ...)
  armv7-unknown-linux-musleabihf   # ARMv7 models (armada38x, alpine, ...)
)

TARGETS=("$@")
[[ ${#TARGETS[@]} -eq 0 ]] && TARGETS=("${ALL_TARGETS[@]}")

cd "$(dirname "$0")"

for target in "${TARGETS[@]}"; do
  if ! rustup target list --installed | grep -qx "$target"; then
    echo "[INFO] Installing Rust target $target …"
    rustup target add "$target"
  fi

  linker_var="CARGO_TARGET_$(echo "$target" | tr 'a-z-' 'A-Z_')_LINKER"
  echo "[INFO] Building $BIN for $target …"
  env "$linker_var=rust-lld" cargo build --release --target "$target" --bin "$BIN"
  file "target/$target/release/$BIN"
done
