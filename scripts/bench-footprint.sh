#!/bin/sh
# Frozen memory+latency ruler for the transcribe CLI.
# Usage: scripts/bench-footprint.sh [runs] [wav]
# Emits one TSV line per run: peak_footprint_mb, max_rss_mb, transcription_ms, wall_ms, transcript.
set -eu
root="$(cd "$(dirname "$0")/.." && pwd)"
bin="$root/src-tauri/target/release/see-computer"
runs="${1:-3}"
wav="${2:-$root/fixtures/sv.wav}"
i=0
printf 'peak_footprint_mb\tmax_rss_mb\ttranscription_ms\twall_ms\ttranscript\n'
while [ "$i" -lt "$runs" ]; do
  out="$(/usr/bin/time -l "$bin" transcribe "$wav" 2>&1)"
  peak="$(printf '%s\n' "$out" | awk '/peak memory footprint/ {printf "%.1f", $1/1048576}')"
  rss="$(printf '%s\n' "$out" | awk '/maximum resident set size/ {printf "%.1f", $1/1048576}')"
  ms="$(printf '%s\n' "$out" | awk '/^transcription:/ {print $2}')"
  wall="$(printf '%s\n' "$out" | awk '/^wall:/ {print $2}')"
  text="$(printf '%s\n' "$out" | grep -v -E '^(audio|model|transcription|wall|nothing)|real|maximum|average|page|instruction|cycles|peak|swap|involuntary|voluntary|block|signals|context|messages' | tail -1)"
  printf '%s\t%s\t%s\t%s\t%s\n' "$peak" "$rss" "$ms" "$wall" "$text"
  i=$((i + 1))
done
