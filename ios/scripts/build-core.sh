#!/bin/sh
# Xcode's build phase for the Rust engine. It always builds release, because a
# debug engine takes many times longer to transcribe and measures nothing.
set -eu

case "${PLATFORM_NAME:-iphoneos}" in
  iphonesimulator) target=aarch64-apple-ios-sim ;;
  *) target=aarch64-apple-ios ;;
esac

export PATH="$HOME/.cargo/bin:$PATH"
root="$(cd "$(dirname "$0")/.." && pwd)"
unset IPHONEOS_DEPLOYMENT_TARGET SDKROOT
exec cargo build --release --target "$target" --manifest-path "$root/core/Cargo.toml"
