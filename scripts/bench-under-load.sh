#!/bin/sh
# What one dictation costs while the machine is already busy.
# Usage: scripts/bench-under-load.sh [runs] [hogs] [wav]
# Emits one transcription-millisecond figure per run, then the median.
set -eu
root="$(cd "$(dirname "$0")/.." && pwd)"
bin="$root/src-tauri/target/release/see-computer"
runs="${1:-7}"
hogs="${2:-10}"
wav="${3:-$root/fixtures/sv.wav}"

if [ "$hogs" -gt 0 ]; then
  swift "$root/scripts/busy.swift" "$hogs" >/dev/null &
  busy=$!
  trap 'kill "$busy" 2>/dev/null || true' EXIT INT TERM
  sleep 2
fi

i=0
while [ "$i" -lt "$runs" ]; do
  "$bin" transcribe "$wav" 2>&1 >/dev/null | awk '/^transcription:/ {print $2}'
  i=$((i + 1))
done | sort -n | awk '{v[NR]=$1; print "run\t" $1} END {print "median\t" v[int((NR+1)/2)]}'
