#!/bin/sh
# Usage: typing-context.sh UDID corpus-directory [dev|test]
set -eu
scripts="$(cd "$(dirname "$0")" && pwd)"
device=${1:?Pass a booted simulator UDID}
data=${2:?Pass the directory built by build-context-eval.py}
data="$(cd "$data" && pwd)"
split=${3:-dev}
build="$(mktemp -d)"
trap 'rm -rf "$build"' EXIT
"$scripts/build-typing-sim.sh" "$build"
export TYPING_SIM_BIN="$build/sim"
for source in context dev; do
    for lang in en sv; do
        language=english
        if [ "$lang" = sv ]; then language=swedish; fi
        set -- --languages "$language"
        if [ "$source" = context ]; then set -- "$@" --input "$data/$split-$lang.txt"; fi
        echo "$source $lang"
        "$scripts/typing-sim.sh" "$device" "$@"
        "$scripts/typing-sim.sh" "$device" "$@" --mode suggest
    done
done
"$scripts/test-typing.sh" "$device"
