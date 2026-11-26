#!/bin/sh
set -eu
root="$(cd "$(dirname "$0")/.." && pwd)"
scratch=${1:?Pass an output directory}
cp "$root"/Keyboard/words-*.bin "$scratch/"
"$root/scripts/typing-swiftc.sh" "$scratch" -O \
    -import-objc-header "$root/Keyboard/DocumentIdentifier.h" \
    "$root/Shared/Lexicon.swift" "$root/Keyboard/TapDecoder.swift" "$root/Keyboard/SwipeDecoder.swift" "$root/Shared/Bridge.swift" "$root/Shared/TypingLanguages.swift" \
    "$root"/Keyboard/TypingVocabulary.swift "$root/Keyboard/Typist.swift" \
    "$root/Keyboard/TypingKeys.swift" "$root/Keyboard/KeyboardControls.swift" \
    "$root/tests/TypingSim.swift" "$root/tests/TypingLogReplay.swift" -o "$scratch/sim"
