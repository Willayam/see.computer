#!/usr/bin/env python3
"""Summarise a typing log from pull-typing-log.sh: the fields typed into, every
autocorrection, corrections the user undid, words the user deleted and retyped, and
swipes. Usage: typing-log-report.py typing-log.jsonl [--since YYYY-MM-DD]"""
import json, re, sys
from collections import Counter
from datetime import datetime, timezone

KEYBOARD = {0: "default", 1: "ascii", 2: "numbers+punct", 3: "url", 4: "number", 5: "phone",
            6: "name+phone", 7: "email", 8: "decimal", 9: "twitter", 10: "websearch", 11: "ascii-number"}
TRAIT = {0: "default", 1: "no", 2: "yes"}
CAP = {0: "none", 1: "words", 2: "sentences", 3: "all"}


def words(text):
    return re.findall(r"[\w'’]+", text or "")


def last_word(text):
    found = words(text)
    return found[-1] if found else ""


def main():
    path = sys.argv[1]
    since = None
    if "--since" in sys.argv:
        since = datetime.fromisoformat(sys.argv[sys.argv.index("--since") + 1]).replace(tzinfo=timezone.utc).timestamp() * 1000
    events = [json.loads(line) for line in open(path, encoding="utf-8") if line.strip()]
    if since:
        events = [e for e in events if e["t"] >= since]

    fields = Counter()
    corrections, undone, retyped, swipes = [], [], [], []
    deleting = None  # (word before the deletes began, context)
    for e in events:
        kind = e["e"]
        if kind == "field":
            fields[(e.get("host") or "?", KEYBOARD.get(e["keyboard"], e["keyboard"]), TRAIT.get(e["autocorrect"], e["autocorrect"]),
                    CAP.get(e["autocap"], e["autocap"]), e.get("content") or "-", "corrects" if e["corrects"] else "NO CORRECTION")] += 1
            continue
        b, a = e.get("b"), e.get("a")
        if b is None or a is None:
            continue
        if kind == "key":
            out = e["o"]
            if a != b + out and out in " .,!?;:\n" and words(b):
                corrections.append((last_word(b), words(a)[-1] if words(a) else a, b[-50:]))
            if deleting and out.strip() and not out.isalpha() or (deleting and out == " "):
                typed = last_word(b if out == " " else a)
                if typed and typed != deleting[0]:
                    retyped.append((deleting[0], typed, deleting[1]))
                deleting = None
        elif kind == "delete":
            if len(b) - len(a) > 1 or not b.startswith(a):
                undone.append((last_word(b), last_word(a), a[-50:]))
            elif deleting is None and b and (b[-1] == " " or b[-1].isalpha()):
                deleting = (last_word(b), b[-50:])
        elif kind == "swipe":
            cands = [c[0] for c in e.get("c", [])]
            swipes.append((cands[:3], e.get("kept", True), len(e.get("p", []))))
        elif kind == "pick":
            corrections.append((last_word(b), e["w"] + "  (picked)", b[-50:]))

    print(f"{len(events)} events\n\nFields (host, keyboard, autocorrect, autocapitalize, content type, correction):")
    for key, count in fields.most_common():
        print(f"  {count:4d}  {'  '.join(map(str, key))}")
    print(f"\nAutocorrections and picks ({len(corrections)}):")
    for typed, became, context in corrections[-60:]:
        if typed != became:
            print(f"  {typed!r} -> {became!r}   …{context}")
    print(f"\nCorrections the user undid with delete ({len(undone)}):")
    for was, back, context in undone[-40:]:
        print(f"  {was!r} -> {back!r}   …{context}")
    print(f"\nWords deleted and retyped ({len(retyped)}):")
    for first, second, context in retyped[-60:]:
        print(f"  {first!r} -> {second!r}   …{context}")
    print(f"\nSwipes ({len(swipes)}):")
    for cands, kept, points in swipes[-40:]:
        print(f"  {'/'.join(cands)}  points={points}{'' if kept else '  (rejected as a tap)'}")


if __name__ == "__main__":
    main()
