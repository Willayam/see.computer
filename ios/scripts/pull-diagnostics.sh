#!/bin/sh
# Copies the diagnostics log off the connected iPhone. The app and the keyboard
# both append to it in the App Group folder.
# Usage: pull-diagnostics.sh [destination] [device id]
set -eu

destination="${1:-./diagnostics-iphone.tsv}"
device="${2:-$(xcrun devicectl list devices --hide-default-columns --columns Identifier --filter "hardwareProperties.reality == 'physical' AND hardwareProperties.deviceType == 'iPhone'" --hide-headers 2>/dev/null | head -1 | awk '{print $1}')}"

if [ -z "$device" ]; then
  echo "No iPhone found. Connect it, unlock it, and try again." >&2
  exit 1
fi

xcrun devicectl device copy from --device "$device" \
  --domain-type appGroupDataContainer --domain-identifier group.computer.see.ios \
  --source Library/diagnostics.tsv --destination "$destination"
echo "$destination: $(wc -l < "$destination" | tr -d ' ') events"
