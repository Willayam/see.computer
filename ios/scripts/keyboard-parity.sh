#!/bin/sh
# Compare our keyboard with Apple's, pixel for pixel, in a booted iOS Simulator.
#
#   keyboard-parity.sh setup <udid>          build the keyboard into a host app, install it,
#                                            enable Apple's English and Swedish keyboards and ours
#   keyboard-parity.sh run <udid> <out> [state ...]
#                                            capture each state on both keyboards and compare;
#                                            a name ending in * picks every state it starts
#                                            with. Results go to <out>/summary.tsv, one
#                                            <state>-side.png (Apple | ours | difference)
#                                            and one <state>-caps.tsv per state
#   keyboard-parity.sh states                list the states
#
# AGENT_DEVICE must name the agent-device command bound to the simulator, because every
# tap and screenshot goes through it. The host app takes the bundle id computer.see.ios,
# so it replaces see.computer on that simulator. Use a simulator made for this.
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/.." && pwd)"
build="${PARITY_BUILD:-${TMPDIR:-/tmp}/see-computer-parity}"

ad() { "${AGENT_DEVICE:?Set AGENT_DEVICE to the agent-device command for this simulator}" "$@"; }

container() { xcrun simctl get_app_container "$1" computer.see.ios data; }

setup() {
    udid="$1"
    mkdir -p "$build"
    xcodebuild -project "$root/SeeComputer.xcodeproj" -scheme see.computer -configuration Debug \
        -destination "id=$udid" -derivedDataPath "$build/derived" build > "$build/keyboard.log" 2>&1 \
        || { tail -30 "$build/keyboard.log"; exit 1; }
    app="$build/ParityHost.app"
    rm -rf "$app"
    mkdir -p "$app/PlugIns"
    xcrun --sdk iphonesimulator swiftc -target "$(uname -m)-apple-ios17.0-simulator" \
        -sdk "$(xcrun --sdk iphonesimulator --show-sdk-path)" "$here/parity/ParityHost.swift" -o "$app/ParityHost"
    cp "$here/parity/Host-Info.plist" "$app/Info.plist"
    cp -R "$build/derived/Build/Products/Debug-iphonesimulator/Keyboard.appex" "$app/PlugIns/"
    codesign -s - --force "$app" > /dev/null 2>&1
    xcrun simctl install "$udid" "$app"
    xcrun simctl spawn "$udid" defaults write -g AppleKeyboards -array \
        "en_US@sw=QWERTY;hw=Automatic" "sv_SE@sw=QWERTY-Swedish;hw=Automatic" "emoji@sw=Emoji" "computer.see.ios.keyboard"
    xcrun simctl spawn "$udid" defaults write -g AppleKeyboardsExpanded -int 1
    # Allow Full Access, which Settings stores as a TCC grant.
    tcc="$(xcrun simctl getenv "$udid" HOME)/Library/TCC/TCC.db"
    for client in computer.see.ios computer.see.ios.keyboard; do
        sqlite3 "$tcc" "insert or replace into access (service, client, client_type, auth_value, auth_reason, auth_version, flags)
            values ('kTCCServiceKeyboardNetwork', '$client', 0, 2, 2, 1, 0)"
    done
    echo "Installed. The first time, open the host, tap the field, hold the globe key and pick see.computer once."
}

case "${1:-}" in
    setup) setup "${2:?Pass the simulator UDID}" ;;
    states) python3 "$here/parity/run.py" - - - list ;;
    run)
        udid="${2:?Pass the simulator UDID}"
        out="${3:?Pass an output folder}"
        shift 3
        mkdir -p "$out" "$build"
        swiftc -O "$here/parity/Compare.swift" -o "$build/compare"
        rm -f "$out/summary.tsv"
        ad open computer.see.ios --relaunch > /dev/null
        python3 "$here/parity/run.py" "$udid" "$out" "$build/compare" "$@"
        ;;
    *) sed -n 2,12p "$0"; exit 1 ;;
esac
