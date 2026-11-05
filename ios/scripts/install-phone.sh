#!/bin/sh
# Builds a release and installs it on the connected iPhone. Xcode must be signed
# in to an Apple account on the team named in the project.
set -eu

root="$(cd "$(dirname "$0")/.." && pwd)"
products="${TMPDIR:-/tmp}/see-computer-ios"
device="${1:-$(xcrun devicectl list devices --hide-default-columns --columns Identifier --filter "hardwareProperties.reality == 'physical' AND hardwareProperties.deviceType == 'iPhone'" --hide-headers 2>/dev/null | head -1 | awk '{print $1}')}"

if [ -z "$device" ]; then
  echo "No iPhone found. Connect it, unlock it, and try again." >&2
  exit 1
fi

xcodebuild -project "$root/SeeComputer.xcodeproj" -scheme see.computer -configuration Release \
  -destination "id=$device" -derivedDataPath "$products" -allowProvisioningUpdates build
xcrun devicectl device install app --device "$device" "$products/Build/Products/Release-iphoneos/see.computer.app"
