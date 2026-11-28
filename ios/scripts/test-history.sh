#!/bin/sh
set -eu
root="$(cd "$(dirname "$0")/.." && pwd)"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
xcrun swiftc "$root/App/History.swift" "$root/tests/HistoryChecks.swift" -o "$work/check"
mkdir "$work/stockholm" "$work/utc"
TZ=Europe/Stockholm "$work/check" "$work/stockholm" Europe/Stockholm
TZ=UTC "$work/check" "$work/utc" UTC
