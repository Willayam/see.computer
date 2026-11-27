#!/bin/sh
# Check how the app calls the speech engine, against a stand-in for the Rust worker.
set -eu
root="$(cd "$(dirname "$0")/.." && pwd)"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
xcrun clang -c "$root/tests/EngineStub.c" -o "$scratch/stub.o"
xcrun swiftc -import-objc-header "$root/tests/EngineStub.h" "$scratch/stub.o" \
    "$root/App/Engine.swift" "$root/Shared/Bridge.swift" "$root/tests/EngineChecks.swift" -o "$scratch/check"
"$scratch/check"
