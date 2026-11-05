#!/bin/sh
# Archives a build with no private calls and uploads it to App Store Connect.
# Xcode must be signed in to an account on the team named in the project, and
# the app must already exist in App Store Connect, because only its website can
# create it.
set -eu

root="$(cd "$(dirname "$0")/.." && pwd)"
work="${TMPDIR:-/tmp}/see-computer-testflight"

rm -rf "$work"
mkdir -p "$work"
cat > "$work/options.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>method</key>
	<string>app-store-connect</string>
	<key>destination</key>
	<string>upload</string>
	<key>signingStyle</key>
	<string>automatic</string>
	<key>teamID</key>
	<string>Y6BFTH26P4</string>
	<key>manageAppVersionAndBuildNumber</key>
	<false/>
</dict>
</plist>
PLIST

xcodebuild archive -project "$root/SeeComputer.xcodeproj" -scheme see.computer -configuration Release \
  -destination generic/platform=iOS -archivePath "$work/see.computer.xcarchive" \
  -derivedDataPath "$work/derived" -allowProvisioningUpdates \
  CURRENT_PROJECT_VERSION="$(date +%Y%m%d%H%M)" \
  SWIFT_ACTIVE_COMPILATION_CONDITIONS=STORE GCC_PREPROCESSOR_DEFINITIONS=STORE=1

xcodebuild -exportArchive -archivePath "$work/see.computer.xcarchive" \
  -exportOptionsPlist "$work/options.plist" -exportPath "$work/export" -allowProvisioningUpdates
