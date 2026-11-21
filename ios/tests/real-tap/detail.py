"""Audit continuous TSI, source geometry, proper names, and correction at space."""

import json
import math
import re
import sys
import zipfile
from pathlib import Path

import numpy as np

root = Path(sys.argv[1])


def read(name):
    return json.loads((root / (name + ".json")).read_text())


def clean(s):
    return re.findall(r"[a-z]+(?:'[a-z]+)*", s.lower().replace("’", "'"))


def edits(a, b):
    row = list(range(len(b) + 1))
    for i, x in enumerate(a):
        nxt = [i + 1]
        for j, y in enumerate(b):
            nxt.append(min(nxt[-1] + 1, row[j + 1] + 1, row[j] + (x != y)))
        row = nxt
    return row[-1]


def rate(num, den):
    n, d = np.asarray(num), np.asarray(den)
    draws = 100 * (weights @ n) / (weights @ d)
    return np.round([100 * n.sum() / d.sum(), *np.percentile(draws, [2.5, 97.5])], 2)


layout = read("english")
trials = read("en")
outputs = {o["id"]: o for o in read("see-en")}
errors, count, raw_on, broken, short, short_err = 0, 0, 0, 0, 0, 0
name_n = name_e = misses = aligned = 0
archive = zipfile.ZipFile(root / "word.zip")
for s in trials:
    result = outputs[s["id"]]
    named = set()
    if s["person"].startswith("vertanen"):
        path, i = s["id"].rsplit("/", 1)
        block = archive.read(path).decode().split("\n\n")[int(i)]
        original = re.search(r"^ORIG: (.*)$", block, re.M)
        if original:
            named = {
                w.lower()
                for w in re.findall("[A-Za-z]+", original[1])[1:]
                if w.istitle() and w not in ("I", "Friday")
            }
    for w, p, c, taps in zip(
        s["words"], result["beforeSpace"], result["commits"], s["taps"], strict=True
    ):
        before, after = clean(p) == [w], clean(c) == [w]
        nearest = "".join(
            min(
                (k for k in layout if len(k) == 1),
                key=lambda k: math.dist(pt, layout[k][:2]),
            )
            for pt in taps
        )
        count += before
        errors += before and not after
        raw_on += nearest == w
        broken += nearest == w and not after and before
        short += len(w) <= 3
        short_err += len(w) <= 3 and not after
        name_n += w in named
        name_e += w in named and not after
        if s["person"].startswith("tsi") and len(w) == len(nearest):
            aligned += len(w)
            misses += sum(a != b for a, b in zip(w, nearest))
print(
    "before-space correct then broken",
    errors,
    count,
    "geometric correct then broken specifically at commit",
    broken,
    raw_on,
)
print(
    "short",
    short_err,
    short,
    "proper-name proxy",
    name_e,
    name_n,
    "TSI aligned wrong letters",
    misses,
    aligned,
)
tsi = [s for s in trials if s["person"].startswith("tsi")]
weights = np.random.default_rng(20261121).multinomial(
    len(tsi), [1 / len(tsi)] * len(tsi), size=4000
)
den = [len(s["words"]) for s in tsi]
errors = {}
for name in ("see-en", "free", "aosp-free", "iikb-free"):
    by_id = {r["id"]: r for r in read(name)}
    errors[name] = [edits(s["words"], clean(by_id[s["id"]]["text"])) for s in tsi]
    print("TSI", name, sum(errors[name]), rate(errors[name], den))
    if name == "free":
        failed = []
        recorded = []
        for s in tsi:
            ok = by_id[s["id"]]["insertedSpace"]
            failed.append(
                sum(p is not None and not hit for p, hit in zip(s["spaces"], ok))
            )
            recorded.append(sum(p is not None for p in s["spaces"]))
        print(
            "missed observed spaces", sum(failed), sum(recorded), rate(failed, recorded)
        )
for name in ("see-en", "aosp-free", "iikb-free"):
    print(
        "see observed minus", name, rate(np.subtract(errors["free"], errors[name]), den)
    )
continuous = {s["id"]: s for s in read("continuous")}


def raw_word(taps):
    return "".join(
        min(
            (k for k in layout if len(k) == 1),
            key=lambda k: math.dist(p, layout[k][:2]),
        )
        for p in taps
    )


raw = [
    edits(s["words"], [raw_word(t) for t in continuous[s["id"]]["taps"]]) for s in tsi
]
print("continuous raw", sum(raw), rate(raw, den))


# Exclude trials with any point in the regions occupied by the host's side controls.
# This tests whether an edge-key mismatch explains the donor's apparent advantage.
def interior(p):
    x, y = p
    if y < 0 or y >= 210 or x < 0 or x >= 394:
        return False
    if y >= 157.5:
        return 97.34 <= x <= 295.33
    if y >= 105:
        return 59 <= x <= 335
    return True


keep = np.array(
    [
        all(interior(p) for group in s["taps"] for p in group)
        and all(interior(p) for p in s["spaces"] if p is not None)
        for s in tsi
    ]
)
print(
    "interior",
    keep.sum(),
    np.sum(keep * den),
    "see minus iikb",
    rate(keep * np.subtract(errors["free"], errors["iikb-free"]), keep * den),
)
reset = {r["id"]: r for r in read("iikb-free-reset")}
diff = [
    edits(s["words"], clean(reset[s["id"]]["text"])) - e
    for s, e in zip(tsi, errors["iikb-free"])
]
print("TSI observed reset minus learned", rate(diff, den))
