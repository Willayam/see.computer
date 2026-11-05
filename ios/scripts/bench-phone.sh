#!/bin/sh
# Measures the engine on the connected iPhone. Installs nothing, so run a debug
# build first. Open another app on the phone during the run to measure the
# background numbers too. Usage: bench-phone.sh <device id> <wav> [seconds]
set -eu

device="$1"
wav="$2"
seconds="${3:-45}"
xcrun devicectl device copy to --device "$device" --domain-type appDataContainer \
  --domain-identifier computer.see.ios --source "$wav" --destination Documents/bench.wav >/dev/null
xcrun devicectl device process launch --device "$device" --terminate-existing computer.see.ios -- -seeBench >/dev/null
sleep "$seconds"
out="$(mktemp)"
xcrun devicectl device copy from --device "$device" --domain-type appDataContainer \
  --domain-identifier computer.see.ios --source Documents/bench.jsonl --destination "$out" >/dev/null
cat "$out"
rm -f "$out"
