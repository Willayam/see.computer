"""Score final fields and pre-space bars with paired sentence bootstrap intervals."""

import json
import math
import re
import sys
from collections import Counter
from pathlib import Path

import numpy as np

root = Path(sys.argv[1])


def read(file):
    return json.loads((root / (file + ".json")).read_text())


def distance(a, b):
    row = list(range(len(b) + 1))
    for i, x in enumerate(a):
        nxt = [i + 1]
        for j, y in enumerate(b):
            nxt.append(min(nxt[-1] + 1, row[j + 1] + 1, row[j] + (x != y)))
        row = nxt
    return row[-1]


def normal(text):
    return " ".join(
        re.findall(r"[a-zåäö]+(?:'[a-zåäö]+)*", text.lower().replace("’", "'"))
    )


def ci(estimate, draws):
    lo, hi = np.percentile(draws, (2.5, 97.5))
    return f"{estimate:.2f} [{lo:.2f}, {hi:.2f}]"


for lang, layout, engines in [
    ("en", "english", ["raw", "see", "aosp", "matched", "iikb", "reset"]),
    ("sv", "bilingual", ["raw", "see", "aosp", "iikb"]),
]:
    frames = read(layout)
    all_trials = read(lang)
    results = {
        engine: {r["id"]: r for r in read(engine + "-" + lang)}
        for engine in engines
        if engine != "raw"
    }
    for group in ["tsi", "vertanen", "combined"] if lang == "en" else ["synthetic"]:
        trials = [
            s
            for s in all_trials
            if group == "combined" or s["person"].split("/")[0] == group
        ]
        # Both noise levels for a Swedish sentence travel in the same bootstrap draw.
        clusters = [s["id"].split("/")[-1] if lang == "sv" else s["id"] for s in trials]
        unique = sorted(set(clusters))
        indices = {k: i for i, k in enumerate(unique)}
        rng = np.random.default_rng(20261121)
        weights = rng.multinomial(
            len(unique), np.full(len(unique), 1 / len(unique)), size=4000
        )
        raw_words = []
        for s in trials:

            def nearest(x, y):
                return min(
                    (k for k in frames if len(k) == 1),
                    key=lambda k: math.dist((x, y), frames[k][:2]),
                )

            raw_words.append(["".join(nearest(*p) for p in taps) for taps in s["taps"]])
        print(
            "\n",
            group,
            len(trials),
            "sentences",
            len(set(s["person"] for s in trials)),
            "people/levels",
        )
        rates = {}
        for engine in engines:
            counts = np.zeros((len(unique), 7), dtype=int)
            breakdown, samples = Counter(), []
            for s, raw, cluster in zip(trials, raw_words, clusters, strict=True):
                words = s["words"]
                result = (
                    results[engine][s["id"]]
                    if engine != "raw"
                    else dict(text=" ".join(raw), commits=raw, bars=[[r] for r in raw])
                )
                commits = [normal(w) for w in result["commits"]]
                for known, word, geometric, committed in zip(result.get("known", []), words, raw, commits):
                    if geometric == word:
                        group_name = "in dictionary" if known else "outside dictionary"
                        breakdown[f"{group_name} correct taps"] += 1
                        breakdown[f"{group_name} changed"] += committed != word
                errors = distance(words, normal(result["text"]).split())
                row = counts[indices[cluster]]
                row[0] += len(words)
                row[1] += errors
                for w, r, c, bar in zip(
                    words, raw, commits, result["bars"], strict=True
                ):
                    row[2] += r == w
                    row[3] += r == w and c != w
                    row[4] += r != w
                    row[5] += r != w and w in map(normal, bar[:3])
                    row[6] += w in map(normal, bar[:3])
                    if w != c:
                        breakdown[
                            "broken"
                            if r == w
                            else "unchanged"
                            if r == c
                            else "wrong replacement"
                        ] += 1
                        if len(w) <= 3:
                            breakdown["short"] += 1
                        if len(samples) < 20:
                            samples.append((w, r, c))
            total = counts.sum(axis=0)
            boot = weights @ counts
            metrics = []
            for n, d in [(1, 0), (3, 2), (5, 4), (6, 0)]:
                est = 100 * total[n] / total[d]
                draws = 100 * boot[:, n] / boot[:, d]
                metrics.append(ci(est, draws))
                if n == 1:
                    rates[engine] = (est, draws)
            print(engine, total.tolist(), *metrics, sep=" | ")
            if engine == "see":
                print("breakdown", breakdown, samples[:10])
        for alt in engines:
            est, draws = rates["see"]
            print("see minus", alt, ci(est - rates[alt][0], draws - rates[alt][1]))
        if "reset" in rates:
            print(
                "reset minus learned",
                ci(
                    rates["reset"][0] - rates["iikb"][0],
                    rates["reset"][1] - rates["iikb"][1],
                ),
            )
