#!/bin/bash
set -eu
cd "$(dirname "$0")/../../.."
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
swiftc scripts/tests/library/webkit.swift -o "$work/webkit"
python3 - "$work/rows.json" <<'PY'
import datetime
import json
import sys

rows = []
for index in range(365):
    day = datetime.date(2025, 1, 1) + datetime.timedelta(days=index)
    at = int(datetime.datetime.combine(day, datetime.time(12)).timestamp()) * 1000
    lines = [{"startMs": at + line * 1000, "text": f"fixture {day} {line}"} for line in range(50)]
    rows.append({"id": f"dictation:{day}", "kind": "dictation", "state": "done", "at": at,
                 "title": "50 dictations", "lines": lines, "media": [], "paste": "",
                 "files": {"folder": "/fixture"}})
with open(sys.argv[1], "w") as stream:
    json.dump(rows, stream)
PY
SEE_AUDIT_SCRIPT="$PWD/scripts/tests/library/navigation.js" "$work/webkit" "$PWD/ui" "$work/rows.json" 365
