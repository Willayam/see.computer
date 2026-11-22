#!/bin/bash
# Compile simulator checks with the same pinned static libraries as Keyboard.appex.
set -eu
root="$(cd "$(dirname "$0")/.." && pwd)"
scratch="$1"
shift
runtime="$root/SwipeRuntime"
artifacts="${SWIPE_RUNTIME_ARTIFACTS:-$runtime/.build/artifacts/swiperuntime}"
if [[ ! -d "$artifacts" ]]; then
    swift package --package-path "$runtime" resolve >&2
fi
target="$(uname -m)-apple-ios17.0-simulator"
sdk="$(xcrun --sdk iphonesimulator --show-sdk-path)"
clang_args=(-target "$target" -isysroot "$sdk" -fobjc-arc -O2)
src="$runtime/Sources/SwipeRuntime"
headers="$artifacts/executorch/executorch.xcframework/ios-arm64-simulator/Headers"
xcrun clang "${clang_args[@]}" -I"$headers" -I"$src/include" -c "$src/SwipeEncoder.m" -o "$scratch/SwipeEncoder.o"
echo 'module SwipeRuntime { header "SwipeRuntime.h" export * }' > "$scratch/module.modulemap"
cp "$root"/Keyboard/frequency-*.bin "$src/include/SwipeRuntime.h" "$root/Keyboard/FutoSwipe/encoder.pte" "$scratch/"
libs=("$artifacts"/*/*.xcframework/ios-arm64-simulator/*.a)
exec xcrun --sdk iphonesimulator swiftc -target "$target" -sdk "$sdk" -I "$scratch" "$scratch/SwipeEncoder.o" \
    -Xlinker -all_load "${libs[@]}" -lc++ -framework Accelerate -framework CoreImage -framework CoreVideo \
    "$root/Shared/Diagnostics.swift" "$root/Keyboard/NeuralSwipe.swift" "$@"
