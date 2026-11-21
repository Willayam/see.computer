#!/bin/sh
# Type the held-out sentences on the real keyboard views in a booted iOS Simulator.
# Usage: typing-sim.sh <simulator UDID> [--set dev|test] [--languages bilingual|english] [--out <tsv>]
# Prints one line per noise level, then the metric: word error rate over the two noisy levels.
set -eu
device="${1:?Pass a booted simulator UDID}"
shift
out=""
pass=""
replay=""
layout=""
spaces=""
language=english
metric=noisy_wer
while [ $# -gt 0 ]; do
    case "$1" in
        --mode) pass="$pass --mode $2"; if [ "$2" = swipe ]; then metric=noisy_swipe_wer; fi; shift 2 ;;
        --replay) replay="$2"; shift 2 ;;
        --layout) layout="$2"; shift 2 ;;
        --observed-spaces) spaces=--observed-spaces; shift ;;
        --languages) language="$2"; pass="$pass --languages $2"; shift 2 ;;
        --out) out="$2"; shift 2 ;;
        *) pass="$pass $1"; shift ;;
    esac
done
root="$(cd "$(dirname "$0")/.." && pwd)"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
cp "$root"/Keyboard/words-*.bin "$scratch/"
xcrun --sdk iphonesimulator swiftc -O -target "$(uname -m)-apple-ios17.0-simulator" \
    -sdk "$(xcrun --sdk iphonesimulator --show-sdk-path)" \
    -import-objc-header "$root/Keyboard/DocumentIdentifier.h" \
    "$root/Keyboard/Lexicon.swift" "$root/Keyboard/TapDecoder.swift" "$root/Keyboard/SwipeDecoder.swift" "$root/Shared/Bridge.swift" "$root/Shared/TypingLanguages.swift" \
    "$root"/Keyboard/TypingVocabulary.swift "$root/Keyboard/Typist.swift" \
    "$root/Keyboard/TypingKeys.swift" "$root/Keyboard/KeyboardControls.swift" \
    "$root/tests/TypingSim.swift" -o "$scratch/sim"
if [ -n "$layout" ]; then
    xcrun simctl spawn "$device" "$scratch/sim" --corpus "$root/tests/typing-eval" --languages "$language" --layout "$layout"
    exit
fi
if [ -n "$replay" ]; then
    xcrun simctl spawn "$device" "$scratch/sim" --corpus "$root/tests/typing-eval" --languages "$language" --replay "$replay" --out "${out:?Pass --out with --replay}" $spaces
    exit
fi
for level in clean steady hurried; do
    # shellcheck disable=SC2086
    xcrun simctl spawn "$device" "$scratch/sim" --corpus "$root/tests/typing-eval" --level "$level" \
        --out "$scratch/$level.tsv" $pass > "$scratch/$level.txt" 2>&1 &
done
wait
for level in clean steady hurried; do grep -E "^($level |ten_letter )"  "$scratch/$level.txt" || { cat "$scratch/$level.txt"; exit 1; }; done
cat "$scratch"/clean.txt "$scratch"/steady.txt "$scratch"/hurried.txt | awk -v metric="$metric" '
    /^(steady|hurried) / { for (i = 1; i <= NF; i++) { split($i, kv, "="); if (kv[1] == "errors") e += kv[2]; if (kv[1] == "words") w += kv[2] } }
    END { printf "metric %s=%.3f%%\n", metric, e / w * 100 }'
if [ -n "$out" ]; then
    { head -1 "$scratch/clean.tsv"; for level in clean steady hurried; do tail -n +2 "$scratch/$level.tsv"; done; } > "$out"
fi
