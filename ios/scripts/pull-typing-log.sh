#!/bin/sh
# Copies the keyboard's typing log off the connected iPhone, older half first.
# Usage: pull-typing-log.sh [destination] [device id]
set -eu

destination="${1:-./typing-log.jsonl}"
device="${2:-$(xcrun devicectl list devices --hide-default-columns --columns Identifier --filter "hardwareProperties.reality == 'physical' AND hardwareProperties.deviceType == 'iPhone'" --hide-headers 2>/dev/null | head -1 | awk '{print $1}')}"

if [ -z "$device" ]; then
  echo "No iPhone found. Connect it, unlock it, and try again." >&2
  exit 1
fi

parts="$(mktemp -d)"
for name in typing-log.1.jsonl typing-log.jsonl; do
  xcrun devicectl device copy from --device "$device" \
    --domain-type appGroupDataContainer --domain-identifier group.computer.see.ios \
    --source "Library/$name" --destination "$parts/$name" >/dev/null 2>&1 || true
done
cat "$parts"/typing-log.1.jsonl "$parts"/typing-log.jsonl 2>/dev/null > "$destination" || true
rm -rf "$parts"
echo "$destination: $(wc -l < "$destination" | tr -d ' ') events"
