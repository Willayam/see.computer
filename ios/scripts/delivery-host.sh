#!/bin/sh
# Dictate into real text fields in a booted iOS Simulator and check what each field ends up
# holding. The keyboard's own log cannot show that, because the keyboard sees its guess of
# the field after a plain insert, not the field.
#
#   delivery-host.sh setup <udid>     build the app with the file microphone and a host app
#                                     whose fields ignore marked text or every edit, install
#                                     both, and enable the keyboard with Full Access
#   delivery-host.sh run <udid> [scenario ...]
#                                     run the named scenarios, or all of them, and print PASS
#                                     or FAIL for each with the field's text
#   delivery-host.sh scenarios        list the scenarios
#
# AGENT_DEVICE must name the agent-device command bound to the simulator, because every tap
# goes through it. Use a simulator made for this: the run kills the keyboard process, drops
# the takes still waiting, writes the keyboard's files and dictates fixtures/en.wav over and
# over. The app needs its speech model, so open see.computer once and let the download
# finish before the first run.
#
# Taps on the keyboard go by position. The defaults fit a 402 pt wide iPhone. For another
# screen set DICTATE, CANCEL, STRIP, LETTER, MIC (the app's mic button) and INPUT (the web
# page's field) to "x y".
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/.." && pwd)"
build="${DELIVERY_BUILD:-${TMPDIR:-/tmp}/see-computer-delivery}"
app=computer.see.ios
host=computer.see.deliveryhost
said='Hi, this is a quick dictation test in English[,.] [Ww]e are measuring how fast the text shows up\.'

ad() { "${AGENT_DEVICE:?Set AGENT_DEVICE to the agent-device command for this simulator}" "$@" > /dev/null 2>&1; }
tap() { ad press $1; }

setup() {
    mkdir -p "$build"
    xcodebuild -project "$root/SeeComputer.xcodeproj" -scheme see.computer -configuration Debug \
        -destination "id=$udid" -derivedDataPath "$build/derived" \
        SWIFT_ACTIVE_COMPILATION_CONDITIONS='DEBUG STORE' GCC_PREPROCESSOR_DEFINITIONS='DEBUG=1 STORE=1' \
        build > "$build/app.log" 2>&1 || { tail -30 "$build/app.log"; exit 1; }
    xcrun simctl install "$udid" "$build/derived/Build/Products/Debug-iphonesimulator/see.computer.app"
    cp "$root/../fixtures/en.wav" "$(xcrun simctl get_app_container "$udid" $app data)/Documents/test-input.wav"
    bundle="$build/DeliveryHost.app"
    rm -rf "$bundle"
    mkdir -p "$bundle"
    xcrun --sdk iphonesimulator swiftc -target "$(uname -m)-apple-ios17.0-simulator" \
        -sdk "$(xcrun --sdk iphonesimulator --show-sdk-path)" "$here/delivery/DeliveryHost.swift" -o "$bundle/DeliveryHost"
    cp "$here/delivery/Host-Info.plist" "$bundle/Info.plist"
    codesign -s - --force "$bundle" > /dev/null 2>&1
    xcrun simctl install "$udid" "$bundle"
    xcrun simctl spawn "$udid" defaults read -g AppleKeyboards 2> /dev/null | grep -q computer.see.ios.keyboard \
        || xcrun simctl spawn "$udid" defaults write -g AppleKeyboards -array-add computer.see.ios.keyboard
    # Allow Full Access, which Settings stores as a TCC grant.
    tcc="$(xcrun simctl getenv "$udid" HOME)/Library/TCC/TCC.db"
    for client in $app $app.keyboard; do
        sqlite3 "$tcc" "insert or replace into access (service, client, client_type, auth_value, auth_reason, auth_version, flags)
            values ('kTCCServiceKeyboardNetwork', '$client', 0, 2, 2, 1, 0)"
    done
    echo "Installed. Pick see.computer once from the globe key in the host's single field, so other apps use it too."
}

# What the run reads: the keyboard's log, the files it shares with the app, and the host's own record of its fields.
places() {
    group="$(xcrun simctl get_app_container "$udid" $app group.computer.see.ios)"
    fields="$(xcrun simctl get_app_container "$udid" $host data)/Documents"
    log="$group/Library/diagnostics.tsv"
}

since() { tail -n +$((mark + 1)) "$log"; }
note() { mark=$(wc -l < "$log"); }
# Whether the keyboard or the app has logged the event since the last note.
saw() { since | cut -f3 | grep -qx "$1"; }
await() {
    tries=0
    until saw "$1"; do
        sleep 0.25
        tries=$((tries + 1))
        [ $tries -lt $((${2:-20} * 4)) ] || { echo "   never saw $1"; return 1; }
    done
}
tell() { told=$((told + 1)); echo "$1&id=$$-$told" > "$fields/command.txt"; sleep "${2:-0.7}"; }
text() { awk -F'\t' -v f="$1" '$1 == f { print $4 }' "$fields/fields.tsv"; }
marked() { awk -F'\t' -v f="$1" '$1 == f { print $3 }' "$fields/fields.tsv"; }
keyboard() { xcrun simctl spawn "$udid" launchctl list 2> /dev/null | awk '/computer.see.ios.keyboard/ { print $1 }'; }
events() { since | awk -F'\t' '{ print "   " $2, $3, $4 }' | grep -E "inserted|placed|unplaced|refused|sent_seen|offer_dismissed|keyboard_replaced|words_kept" || true; }

# Brings an app forward and waits for the keyboard to say it is showing there.
front() { shown=$(grep -c keyboard_shown "$log"); ad open "$@"; }
showing() {
    tries=0
    while [ "$(grep -c keyboard_shown "$log")" = "$shown" ] && [ $tries -lt 160 ]; do sleep 0.25; tries=$((tries + 1)); done
    sleep 1.5
}
focus() { ad open $host; sleep 1; tell "blur=1" 0.8; shown=$(grep -c keyboard_shown "$log"); tell "${2:-}focus=$1" 0.1; showing; }
begin() {
    note
    for attempt in 1 2 3; do
        tap "$dictate"
        if await start_sent 6 > /dev/null; then await take_started 10; return; fi
    done
    echo "   the take did not start"
    return 1
}
finish() {
    for attempt in 1 2 3; do
        tap "$dictate"
        if await stop_sent 6 > /dev/null; then await "transcript_delivered\|transcript_empty" 30; sleep "${1:-3}"; return; fi
    done
    echo "   the take did not stop"
    return 1
}
words() {
    tries=0
    until [ "$(marked "$1")" -ge 30 ] 2> /dev/null; do
        sleep 0.3
        tries=$((tries + 1))
        [ $tries -lt 60 ] || { echo "   no live words in $1"; return 1; }
    done
}
arm() {
    ad open $app --relaunch
    sleep 6
    grep -q '"phase":"armed"' "$group/state.json" || { tap "$mic"; sleep 3; }
}
# Takes left waiting or on offer by an earlier run would be offered in this one's fields.
forget() { ad close $app; sleep 1; rm -f "$group/state.json" "$group/ledger.json" "$group/words.json"; }
# Passes when the field's text $2 is $3 and, with $4, the event $4 was logged since the last note.
verdict() {
    if [ -n "${4:-}" ] && ! saw "$4"; then echo "FAIL $1: no $4 in the log, field [$2]"
    elif echo "$2" | grep -Eq "^$3\$"; then echo "PASS $1: [$2]"; return
    else echo "FAIL $1: [$2]"; fi
    fails=$((fails + 1))
}
# Passes when the field's text $2 is exactly $3. A take that ends early says less than the fixture.
same() { if [ "$2" = "$3" ]; then echo "PASS $1: [$2]"; else echo "FAIL $1: [$2], expected [$3]"; fails=$((fails + 1)); fi; }
safari() { told=$((told + 1)); front com.apple.mobilesafari "http://localhost:$port/page.html?$$-$told"; sleep 3; tap "$input"; showing; }
page() { "$AGENT_DEVICE" snapshot -i 2> /dev/null | sed -n 's/.*input\[[0-9]*\]=\(.*\)/\1/p' | head -1; }

plain_empty() { focus plain "clear=1&"; begin && sleep 6 && finish; events; verdict plain_empty "$(text plain)" "$said"; }
plain_text() { focus plain "clear=1&set=plain&text=Existing text.&"; begin && sleep 6 && finish; events; verdict plain_text "$(text plain)" "Existing text\. $said"; }
single_text() { focus single "clear=1&set=single&text=Existing text.&"; begin && sleep 6 && finish; events; verdict single_text "$(text single)" "Existing text\. $said"; }
# The field shows no marked text. Before, an empty one read as sent and a filled one took nothing.
nomark_empty() { focus nomark "clear=1&"; begin && sleep 6 && finish; events; verdict nomark_empty "$(text nomark)" "$said" mark_refused; }
nomark_text() { focus nomark "clear=1&set=nomark&text=Existing text.&"; begin && sleep 6 && finish; events; verdict nomark_text "$(text nomark)" "Existing text\. $said" mark_refused; }
noop_copy() {
    focus noop "clear=1&"; begin && sleep 6 && finish; events
    verdict noop_field "$(text noop)" ""
    note; tap "$strip"; await dictation_copied 5 || return 1
    if xcrun simctl pbpaste "$udid" | grep -Eq "^$said\$"; then
        echo "PASS noop_copy: the clipboard holds every word"
    else
        echo "FAIL noop_copy: the clipboard does not hold the take"; return 1
    fi
}
partial_copy() {
    rm -f "$fields/partial.txt"
    focus plain "clear=1&partial=12&set=plain&text=Agenda.&"
    begin && sleep 6 && finish || return 1
    [ -s "$fields/partial.txt" ] || { echo "FAIL partial_copy: host recorded no partial draft"; return 1; }
    partial="$(cat "$fields/partial.txt")"
    same partial_field "$(text plain)" "$partial"
    note; tap "$strip"; await dictation_copied 5 || return 1
    same partial_copy "$(xcrun simctl pbpaste "$udid")" "$(newest_take)"
    same partial_field_after_copy "$(text plain)" "$partial"
}
# A key typed mid-take keeps the words shown so far, and Done adds only the rest.
typed_midtake() {
    focus plain "clear=1&set=plain&text=Existing text.&"; begin && words plain && tap "$letter" && sleep 4 && finish; events
    verdict typed_midtake "$(text plain)" "Existing text\. Hi, this is a quick dictation test[^H]*shows up\."
}
# The host empties the field mid-take, as a chat app does when it sends.
sent_midtake() {
    focus plain "clear=1&"; begin && words plain && sleep 1.5 && tell "clear=1" 1 && await transcript_delivered 20 && sleep 2; events
    verdict sent_midtake "$(text plain)" "" sent_seen
}
cancelled() { focus plain "clear=1&set=plain&text=Existing text.&"; begin && words plain && tap "$cancel" && await take_cancelled 10 && sleep 2.5; verdict cancelled "$(text plain)" "Existing text\."; }
# The take began on a selection the user then dropped, so it cannot be placed and is offered.
offer() { focus plain "clear=1&set=plain&text=Existing words here&"; tell "select=plain" 1; begin && sleep 3 && tell "caret=plain" 0.5 && sleep 3 && finish; events; }
offered() { offer; verdict offered_first "$(text plain)" "Existing words here"; note; tap "$strip"; sleep 2; events; verdict offered "$(text plain)" "Existing words here $said"; }
dismissed() { offer; note; tap "$cancel"; sleep 2; events; verdict dismissed "$(text plain)" "Existing words here" offer_dismissed; }
# The offer is still there when the keyboard has hidden and come back, with the field under a new identifier.
reoffered() { offer; focus plain; note; tap "$strip"; sleep 2; events; verdict reoffered "$(text plain)" "Existing words here $said"; }
# The user leaves mid-take, ends the take from Safari and comes back to its field.
returned() {
    focus plain "clear=1&set=plain&text=Alpha.&"; begin && words plain && safari && finish 2
    note; front $host; showing; sleep 4; events; verdict returned "$(text plain)" "Alpha\. $said"
    # iOS built a new keyboard for the return, and that one has to be the one that types.
    tap "$letter"; sleep 1; verdict returned_key "$(text plain)" "Alpha\. $said ?[Aa]"
}
# The same, with a second take dictated and placed in Safari while the first one waits.
overlapped() {
    focus plain "clear=1&set=plain&text=Alpha.&"; begin && words plain && safari && finish 2 && begin && sleep 6 && finish; events
    verdict overlapped_safari "$(page)" "$said"
    note; front $host; showing; sleep 4; events; verdict overlapped "$(text plain)" "Alpha\. $said"
}
# The user leaves mid-take for an app with no keyboard. The take ends while they are away,
# by the Dynamic Island's Done or because iOS suspended the keyboard without telling it, and
# the app then stops the take. Its text is in the field when they come back.
away() {
    focus plain "clear=1&set=plain&text=Alpha.&"; begin && words plain
    note; ad open com.apple.Preferences; sleep 4
    island_done; took="$(newest_take)"
    note; ad open $host; await "inserted\|unplaced" 20; sleep 3; events
    same away "$(text plain)" "Alpha. $took"
}
# The user hides the keyboard mid-take, the take ends, and the keyboard process dies before
# the keyboard comes back. The next one has only what the first one saved. The host starts
# again too, because iOS often puts Apple's keyboard in a host whose keyboard was killed.
restarted() {
    focus plain "clear=1&set=plain&text=Alpha.&"; begin && words plain; old="$(keyboard)"
    note; tell "blur=1" 0.1; await keyboard_hidden 10; island_done; was="$(text plain)"; took="$(newest_take)"
    kill -9 "$(keyboard)" 2> /dev/null || true; reopened "$was"
    [ "$(keyboard)" != "$old" ] && same restarted "$(text plain)" "Alpha. $took"
}
island_done() { xcrun simctl spawn "$udid" notifyutil -p computer.see.ios.stop; await transcript_delivered 30; sleep 1; }
newest_take() { python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["takes"][-1]["text"])' "$group/state.json"; }
# The files a build before the ledger left: one finished take, one draft, one bookmark.
legacy() {
    for bookmark in waiting typed; do
        left $bookmark "Alpha. Legacy words from before the update"
        if [ $bookmark = waiting ]; then end=", now complete\."; else end=""; fi
        if [ -f "$group/ledger.json" ] && [ ! -f "$group/inserted.json" ]; then
            verdict legacy_$bookmark "$(text plain)" "Alpha\. Legacy words from before the update$end"
        else
            verdict legacy_$bookmark "the keyboard did not take over the old files" "taken over"
        fi
    done
    arm
}
unanswered() {
    sent='Sent just before the keyboard went away.'
    for existing in "Alpha. $sent" "Alpha."; do
        left unanswered "$existing"
        same unanswered_field "$(text plain)" "$existing"
        note; tap "$strip"; await dictation_copied 5 || return 1
        same unanswered_copy "$(xcrun simctl pbpaste "$udid")" "$sent"
        same unanswered_keeps_field "$(text plain)" "$existing"
    done
    arm
}
# Opens the host on a field holding $2, with the shared files as an earlier keyboard left them.
left() {
    ad close $app; sleep 1; kill -9 "$(keyboard)" 2> /dev/null || true; sleep 1
    python3 "$here/delivery/left-behind.py" "$group" $1
    reopened "$2"
}
# Starts the host again and brings up a new keyboard in a field holding $1.
reopened() {
    ad open $host --relaunch; sleep 3
    note; shown=$(grep -c keyboard_shown "$log"); tell "clear=1&set=plain&text=$1&focus=plain" 0.5; showing; sleep 5; events
}

all="plain_empty plain_text single_text nomark_empty nomark_text noop_copy partial_copy typed_midtake sent_midtake cancelled offered dismissed reoffered returned overlapped away restarted legacy unanswered"
case "${1:-}" in
    setup) udid="${2:?Pass the simulator UDID}"; setup ;;
    scenarios) echo "$all" | tr ' ' '\n' ;;
    run)
        udid="${2:?Pass the simulator UDID}"
        shift 2
        dictate="${DICTATE:-336 563}" cancel="${CANCEL:-256 563}" strip="${STRIP:-110 563}" letter="${LETTER:-21 662}"
        mic="${MIC:-38 84}" input="${INPUT:-199 90}" port="${PORT:-8731}" fails=0 told=0
        places
        # Safari needs a page to type into. The server lives as long as this run.
        python3 -m http.server "$port" --bind 127.0.0.1 --directory "$here/delivery" > /dev/null 2>&1 &
        server=$!
        trap 'kill $server 2> /dev/null' EXIT
        forget
        arm
        for scenario in ${@:-$all}; do
            echo "### $scenario"
            $scenario || { echo "FAIL $scenario: did not run to its end"; fails=$((fails + 1)); }
        done
        [ $fails = 0 ]
        ;;
    *) sed -n 2,22p "$0"; exit 1 ;;
esac
