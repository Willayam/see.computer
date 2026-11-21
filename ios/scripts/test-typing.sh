#!/bin/sh
# Run the keyboard's dictionary checks on a booted iOS Simulator.
set -eu
device="${1:?Pass a booted simulator UDID}"
root="$(cd "$(dirname "$0")/.." && pwd)"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
cp "$root"/Keyboard/words-*.bin "$scratch/"
xcrun --sdk iphonesimulator swiftc -target "$(uname -m)-apple-ios17.0-simulator" \
    -sdk "$(xcrun --sdk iphonesimulator --show-sdk-path)" \
    -import-objc-header "$root/Keyboard/DocumentIdentifier.h" \
    "$root/Keyboard/Lexicon.swift" "$root/Keyboard/TapDecoder.swift" "$root/Keyboard/SwipeDecoder.swift" "$root/Shared/Bridge.swift" "$root/Shared/TypingLanguages.swift" \
    "$root/Keyboard/TypingVocabulary.swift" "$root/Keyboard/Typist.swift" "$root/tests/TypingVocabularyChecks.swift" \
    -o "$scratch/check"
xcrun simctl spawn "$device" "$scratch/check"
