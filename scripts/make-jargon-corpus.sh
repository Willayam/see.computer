#!/bin/sh
# Synthesise an eval manifest from scripts/jargon-sentences.tsv with say(1),
# once clean and once quiet with noise mixed in, since weak audio is where
# boosting misfires. Usage: scripts/make-jargon-corpus.sh <out-dir> [voice...]
set -eu
root="$(cd "$(dirname "$0")/.." && pwd)"
out="${1:?usage: $0 out-dir [voice...]}"
shift
[ "$#" -gt 0 ] || set -- "Daniel" "Eddy (English (US))"
mkdir -p "$out"
manifest="$out/manifest.tsv"
: > "$manifest"
tab="$(printf '\t')"
n=0
for voice in "$@"; do
  slug="$(printf '%s' "$voice" | tr -cd 'A-Za-z')"
  grep -v '^#' "$root/scripts/jargon-sentences.tsv" | while IFS="$tab" read -r reference spoken; do
    [ -n "$reference" ] || continue
    n=$((n + 1))
    base="$out/$slug-$(printf '%02d' "$n")"
    say -v "$voice" -o "$base.aiff" "${spoken:-$reference}"
    ffmpeg -loglevel error -y -i "$base.aiff" -ar 16000 -ac 1 -c:a pcm_s16le "$base.wav"
    ffmpeg -loglevel error -y -i "$base.wav" -f lavfi -i "anoisesrc=color=pink:amplitude=0.02:sample_rate=16000" \
      -filter_complex "[0:a]volume=0.12[v];[v][1:a]amix=inputs=2:duration=first:normalize=0" \
      -ar 16000 -ac 1 -c:a pcm_s16le "$base-weak.wav"
    rm -f "$base.aiff"
    printf '%s\t%s\n' "$(basename "$base.wav")" "$reference" >> "$manifest"
    printf '%s\t%s\n' "$(basename "$base-weak.wav")" "$reference" >> "$manifest"
  done
done
printf '%s\n' "$manifest"
