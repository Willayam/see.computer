"""Score the two-layout English beam alongside Swift's bilingual geometric candidates.

Use merge-swipe-layouts.py for --neural and typing-sim.sh --mode swipe-geometry
for --geometry. Both must replay the same gestures in the same order.
"""

import argparse
import csv
import gzip
import importlib
import json
from pathlib import Path

bench = importlib.import_module("futo-swipe-bench")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("neural", "geometry", "input", "out"):
        parser.add_argument("--" + name, required=True)
    args = parser.parse_args()
    keyboard = Path(__file__).resolve().parents[1] / "Keyboard"
    lex = bench.Lexicon(keyboard / "words-en.bin")
    swedish_lex = bench.Lexicon(keyboard / "words-sv.bin")
    swedish_frequency = (keyboard / "frequency-sv.bin").read_bytes()
    frequency = (keyboard / "frequency-en.bin").read_bytes()
    with (
        gzip.open(args.neural, "rt") as neural,
        open(args.geometry) as geometry,
        open(args.input) as records,
        open(args.out, "w") as output,
    ):
        writer = csv.writer(output, delimiter="\t", lineterminator="\n")
        writer.writerow(["id", "word", "top1"])
        for row, raw_geo, record in zip(neural, geometry, records, strict=True):
            target, _, weights, beams, *_ = json.loads(row)
            geo = json.loads(raw_geo)
            if geo["id"] != json.loads(record)["id"]:
                raise ValueError("Geometric and neural replays must use the same order")
            scores = {}
            for word, mass, prior, _ in beams[0]:
                length = max(1, len(word.replace("'", "")))
                scores[word] = (mass / length**0.45 + 1.25 * length + 0.4 * prior
                                + 0.5 * weights[0] + 0.005 * frequency[lex.ids[word]])
            lists = geo["strict"]
            if scores:
                for word in scores:
                    if "'" in word and word.replace("'", "") in scores:
                        scores[word] -= 0.25
                top = min(scores, key=lambda w: (-scores[w], w))
                def best(rows):
                    return min(rows, key=lambda r: (-r["score"], r["word"])) if rows else None
                anchor = best(lists[0])
                swedish = best(lists[1])
                additions = []
                if swedish:
                    sv = swedish["score"] + weights[1]
                    en = anchor["score"] + weights[0] if anchor else -45
                    margin = (2 - 0.65 * (scores[top] - 0.75 * len(top))
                              + 0.3 * (weights[1] - weights[0])
                              + 0.9 * (max(-60, sv) - weights[1]) / len(swedish["word"])
                              + 0.06 * (max(-45, sv) - max(-45, en))
                              - (1.7 if anchor and anchor["word"].lower() == top else 0))
                    native_scores = []
                    for word, mass, prior, _ in beams[1]:
                        length = max(1, len(word.replace("'", "")))
                        native_scores.append(mass / length**0.45 + 1.25 * length + 0.4 * prior
                                             + 0.5 * weights[1] + 0.005 * swedish_frequency[swedish_lex.ids[word]])
                    evidence = max(native_scores, default=float("-inf"))
                    if (evidence >= scores[top] - 1 or weights[1] > weights[0] + 1) and (not anchor or sv > en):
                        margin = max(0.1, margin)
                    additions.append((lists[1], scores[top] + margin - swedish["score"]))
            else:
                additions = zip(lists if any(lists) else geo["relaxed"], weights, strict=True)
            for candidates, offset in additions:
                for r in candidates:
                    word = r["word"].lower()
                    scores[word] = max(scores.get(word, float("-inf")), r["score"] + offset)
            winner = min(scores, key=lambda w: (-scores[w], w)) if scores else ""
            writer.writerow([geo["id"], target, winner])


if __name__ == "__main__":
    main()
