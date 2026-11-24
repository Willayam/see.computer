#!/bin/sh
# Type the held-out sentences on the real keyboard views in a booted iOS Simulator.
# Usage: typing-sim.sh <simulator UDID> [--set dev|test] [--languages bilingual|english|swedish] [--out <tsv>]
# Suggestions: typing-sim.sh <UDID> --mode suggest prints the keystrokes the strip saves and how often
# it offers the next word. Add --input <sentences.txt> to score another file, one sentence per line.
# Real data: typing-sim.sh <UDID> --mode real-swipe --input sample.jsonl --layout qwerty.json --out results.tsv
# Encoder parity: use --mode neural-swipe with the same input, layout and output arguments.
# Synthetic export: typing-sim.sh <UDID> --mode swipe --export-sv <output-prefix>
# Prints one line per noise level, then the metric: word error rate over the two noisy levels.
set -eu
device="${1:?Pass a booted simulator UDID}"
shift
out=""
pass=""
typing_log=""
replay=""
spaces=""
language=english
mode=tap
native_only=""
input=""
layout=""
metric=noisy_wer
while [ $# -gt 0 ]; do
    case "$1" in
        --native-only) native_only="--native-only"; shift ;;
        --input) input="$2"; shift 2 ;;
        --layout) layout="$2"; shift 2 ;;
        --export-sv) export_sv="$2"; shift 2 ;;
        --mode) mode="$2"; pass="$pass --mode $2"; if [ "$2" = swipe ]; then metric=noisy_swipe_wer; fi; shift 2 ;;
        --replay) replay="$2"; shift 2 ;;
        --typing-log) typing_log="$2"; shift 2 ;;
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
"$root/scripts/typing-swiftc.sh" "$scratch" -O \
    -import-objc-header "$root/Keyboard/DocumentIdentifier.h" \
    "$root/Keyboard/Lexicon.swift" "$root/Keyboard/TapDecoder.swift" "$root/Keyboard/SwipeDecoder.swift" "$root/Shared/Bridge.swift" "$root/Shared/TypingLanguages.swift" \
    "$root"/Keyboard/TypingVocabulary.swift "$root/Keyboard/Typist.swift" \
    "$root/Keyboard/TypingKeys.swift" "$root/Keyboard/KeyboardControls.swift" \
    "$root/tests/TypingSim.swift" "$root/tests/TypingLogReplay.swift" -o "$scratch/sim"
if [ "$mode" = real-swipe ] || [ "$mode" = neural-swipe ] || [ "$mode" = swipe-geometry ]; then
    xcrun simctl spawn "$device" "$scratch/sim" --corpus "$root/tests/typing-eval" --mode "$mode" --languages english $native_only \
        --input "${input:?Pass --input sample.jsonl}" --layout "${layout:?Pass --layout qwerty.json}" --out "${out:?Pass --out results.tsv}"
    exit
fi
if [ "$mode" = suggest ]; then
    set --
    if [ -n "$input" ]; then set -- --input "$input"; fi
    # shellcheck disable=SC2086
    xcrun simctl spawn "$device" "$scratch/sim" --corpus "$root/tests/typing-eval" "$@" $pass
    exit
fi
if [ -n "$typing_log" ]; then
    xcrun simctl spawn "$device" "$scratch/sim" --corpus "$root/tests/typing-eval" --typing-log "$typing_log"
    exit
fi
if [ -n "$layout" ]; then
    xcrun simctl spawn "$device" "$scratch/sim" --corpus "$root/tests/typing-eval" --languages "$language" --layout "$layout"
    exit
fi
if [ -n "$replay" ]; then
    xcrun simctl spawn "$device" "$scratch/sim" --corpus "$root/tests/typing-eval" --languages "$language" --replay "$replay" --out "${out:?Pass --out with --replay}" $spaces
    exit
fi
for level in clean steady hurried; do
    set --
    if [ -n "${export_sv:-}" ]; then set -- --export-sv "$export_sv.$level.jsonl"; fi
    # shellcheck disable=SC2086
    xcrun simctl spawn "$device" "$scratch/sim" --corpus "$root/tests/typing-eval" --level "$level" \
        --out "$scratch/$level.tsv" "$@" $pass > "$scratch/$level.txt" 2>&1 &
done
wait
for level in clean steady hurried; do grep -E "^($level |ten_letter )"  "$scratch/$level.txt" || { cat "$scratch/$level.txt"; exit 1; }; done
cat "$scratch"/clean.txt "$scratch"/steady.txt "$scratch"/hurried.txt | awk -v metric="$metric" '
    /^(steady|hurried) / { for (i = 1; i <= NF; i++) { split($i, kv, "="); if (kv[1] == "errors") e += kv[2]; if (kv[1] == "words") w += kv[2] } }
    END { printf "metric %s=%.3f%%\n", metric, e / w * 100 }'
if [ -n "$out" ]; then
    { head -1 "$scratch/clean.tsv"; for level in clean steady hurried; do tail -n +2 "$scratch/$level.tsv"; done; } > "$out"
fi
