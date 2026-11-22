"""Join FUTO and TypingSim output on swipe ID and summarize top-k accuracy.

Usage: python score-real-swipes.py sample.jsonl baseline.tsv model-output.jsonl
For Swedish, use: python score-real-swipes.py traces.jsonl --synthetic model-output.jsonl
The last file uses the historical raw/blend JSONL format. For current parity,
use score-deployed-swipe.py.
"""

import json
import sys
from collections import defaultdict

import numpy as np


def normalize(word):
    return word.lower().replace("’", "'")


def main(source, baseline, neural):
    base = {}
    if baseline == "--synthetic":
        sessions = {}
        with open(source) as f:
            for index, record in enumerate(map(json.loads, f)):
                sessions[index] = record["sentenceID"]
                base[index] = record["baseline"]
    else:
        with open(source) as f:
            sessions = {r["id"]: r["session"] for r in map(json.loads, f)}
        with open(baseline) as f:
            next(f)
            for line in f:
                key, _, *choices = line.rstrip("\n").split("\t")
                base[int(key)] = choices
    with open(neural) as f:
        rows = list(map(json.loads, f))
    assert len(rows) == len(base) == len(sessions)
    assert set(sessions) == set(base) == {r["id"] for r in rows}
    for group in ("all", "IV", "OOV"):
        totals = defaultdict(lambda: np.zeros(7))
        for r in rows:
            if group != "all" and r["iv"] != (group == "IV"):
                continue
            hits = []
            for candidates in (base[r["id"]], r["raw"], r["blend"]):
                choices = [normalize(w) for w in candidates]
                hits.extend(
                    int(bool(r["word"]) and r["word"] in choices[:k]) for k in (1, 3)
                )
            totals[sessions[r["id"]]] += [1, *hits]
        if not totals:
            continue
        counts = np.array(list(totals.values()))
        total = counts.sum(axis=0)
        rng = np.random.default_rng(42)
        replicates = []
        for _ in range(2000):
            draw = counts[rng.integers(len(counts), size=len(counts))].sum(axis=0)
            replicates.append(100 * draw[1:] / draw[0])
        intervals = np.percentile(replicates, [2.5, 97.5], axis=0)
        for i, rate in enumerate(100 * total[1:] / total[0]):
            print(
                group,
                ("baseline", "encoder", "blend")[i // 2],
                (1, 3)[i % 2],
                int(total[0]),
                int(total[1 + i]),
                f"{rate:.2f} [{intervals[0, i]:.2f}, {intervals[1, i]:.2f}]",
            )
        if group == "all":
            delta = np.array(replicates)[:, 4] - np.array(replicates)[:, 0]
            print("paired blend gain interval", np.percentile(delta, [2.5, 97.5]))
    timings = np.array([[r["encoder_ms"], r["beam_ms"]] for r in rows])
    print("encoder, beam, combined p50 / p95 (ms)")
    print(
        np.percentile(np.column_stack([timings, timings.sum(axis=1)]), [50, 95], axis=0)
    )


if __name__ == "__main__":
    main(*sys.argv[1:])
