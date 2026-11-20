#!/bin/sh
set -eu
root="$(cd "$(dirname "$0")/.." && pwd)"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
xcrun swiftc "$root/Shared/Bridge.swift" "$root/tests/DraftChecks.swift" -o "$scratch/check"
"$scratch/check"
