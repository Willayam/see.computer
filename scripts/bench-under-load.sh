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

case "$runs" in ''|*[!0-9]*|0) echo "runs must be a positive integer" >&2; exit 2;; esac
case "$hogs" in ''|*[!0-9]*) echo "hogs must be a non-negative integer" >&2; exit 2;; esac
[ "$runs" -gt 0 ] || exit 2
[ -r "$wav" ] && [ -f "$wav" ] || { echo "WAV is missing or unreadable" >&2; exit 1; }
[ -x "$bin" ] || { echo "Build the release binary first" >&2; exit 1; }
work=$(mktemp -d)
busy=
cleanup() {
  if [ -n "$busy" ]; then kill "$busy" 2>/dev/null || true; wait "$busy" 2>/dev/null || true; fi
  rm -rf "$work"
}
trap cleanup EXIT
trap 'exit 130' INT TERM
if [ "$hogs" -gt 0 ]; then
  swift "$root/scripts/busy.swift" "$hogs" >/dev/null &
  busy=$!
  sleep 2
fi

i=0
while [ "$i" -lt "$runs" ]; do
  i=$((i + 1))
  if ! transcript=$("$bin" transcribe "$wav" 2>"$work/timing") || [ -z "$transcript" ]; then
    echo "Trial $i returned no successful transcription" >&2
    exit 1
  fi
  awk '/^transcription: [0-9]+([.][0-9]+)? ms$/ { print $2; hits++ } END { exit hits != 1 }' \
    "$work/timing" >> "$work/samples" || { echo "Trial $i has no valid inference timing" >&2; exit 1; }
done
sort -n "$work/samples" > "$work/sorted"
awk -v runs="$runs" '{v[NR]=$1; print "run\t" $1}
  END {if (NR != runs) exit 1; print "median\t" (v[int((NR+1)/2)] + v[int((NR+2)/2)]) / 2}' "$work/sorted"
