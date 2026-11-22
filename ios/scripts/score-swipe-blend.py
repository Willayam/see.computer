"""Tune on validation candidates, then evaluate the frozen score on a held-out split."""

import argparse
import gzip
import importlib
import json
from array import array
from collections import defaultdict
from pathlib import Path

import numpy as np
from build_typing_sources import AOSP
from numba import njit

bench = importlib.import_module("swipe-blend-bench")


def frequencies(folder, language):
    if not folder:
        return {}
    values = {}
    with gzip.open(Path(folder) / ("aosp_" + AOSP[language]), "rt") as source:
        for line in source:
            if line.startswith(" word="):
                fields = dict(
                    part.split("=", 1)
                    for part in line.strip().split(",")
                    if "=" in part
                )
                word = fields["word"].lower().replace("’", "'")
                values[word] = max(values.get(word, 0), int(fields["f"]))
    return values


def read(inputs, sources, geo, folder):
    frequency = [frequencies(folder, lang) for lang in ("en", "sv")]
    lexicons = [
        bench.bench.Lexicon("ios/Keyboard/words-" + lang + ".bin")
        for lang in ("en", "sv")
    ]
    contractions = []
    for lex in lexicons:
        by_trace = defaultdict(list)
        for w, i in lex.ids.items():
            if "'" in w:
                by_trace[w.replace("'", "")].append(i)
        contractions.append(by_trace)
    features = array("d")
    offsets, sessions, baseline, targets = [0], [], [], []
    in_vocabulary = []
    words = {}
    for path, source in zip(inputs, sources, strict=True):
        with gzip.open(path) as f, open(source) as records:
            for raw, record in zip(f, records, strict=True):
                target, session, weights, langs, gold_geo, greedy, *extra = json.loads(
                    raw
                )
                r = json.loads(record)
                before = (
                    r.get("before")
                    if "before" in r
                    else " ".join(r["sentence"].split()[: r["word_idx"]])
                )
                previous, _ = bench.context(before or "", lexicons)
                choices = geo.get(r.get("id"), gold_geo or [])
                geometry = [w.lower() for w in choices]
                baseline.append(geometry[0] == target if geometry else False)
                session = session if session is not None else r.get("sentenceID")
                if session is None:
                    raise ValueError(
                        "Paired intervals require a donor session or sentenceID"
                    )
                sessions.append(str(session))
                in_vocabulary.append(any(target in lex.ids for lex in lexicons))
                targets.append(words.setdefault(target, len(words)))

                def add(
                    w, m, prior, uni, lang, kind, geometry=geometry, weights=weights
                ):
                    rank = geometry.index(w) if w in geometry else 9
                    features.extend(
                        (
                            m,
                            max(1, len(w.replace("'", ""))),
                            prior,
                            uni,
                            weights[lang],
                            lang,
                            kind,
                            words.setdefault(w, len(words)),
                            rank,
                            frequency[lang].get(w, 0),
                        )
                    )

                for lang, candidates in enumerate(langs):
                    lex = lexicons[lang]
                    mass = {w: m for w, m, b, u in candidates}
                    for w, m, b, u in candidates:
                        add(w, m, b, u, lang, 0)
                    if greedy and extra:
                        mass.setdefault(greedy, extra[0])
                        if greedy not in lex.ids and all(
                            greedy not in other.ids for other in lexicons
                        ):
                            add(greedy, extra[0], -16.0, -16.0, lang, 2)
                    alt = [
                        (i, m)
                        for w, m in mass.items()
                        for i in contractions[lang].get(w, ())
                    ]
                    if alt:
                        ids = np.array([i for i, m in alt])
                        for (i, m), prior in zip(alt, lex.prior(ids, previous)):
                            add(lex.words[i], m, prior, lex.uni[i], lang, 1)
                # A missing prefix must still count as an incorrect choice.
                if len(features) // 10 == offsets[-1]:
                    add("", -1e9, -16, -16, 0, 0)
                offsets.append(len(features) // 10)
    return (
        np.frombuffer(features).reshape(-1, 10),
        np.array(offsets),
        np.array(targets),
        np.unique(sessions, return_inverse=True)[1],
        np.array(baseline),
        np.array(in_vocabulary),
    )


@njit(cache=True)
def winners(
    rows,
    offsets,
    power,
    lm,
    freq,
    length,
    language,
    contractions,
    oov,
    geo,
    english,
    aosp,
    excluded=None,
):
    result = np.full(len(offsets) - 1, -1)
    for i in range(len(result)):
        best = -np.inf
        for row in rows[offsets[i] : offsets[i + 1]]:
            m, n, b, u, w, lang, kind, word, rank, f = row
            if excluded is not None and word in excluded[i]:
                continue
            if (
                (kind == 1 and not contractions)
                or (kind == 2 and not oov)
                or (lang and english)
            ):
                continue
            if kind == 2:
                b = u = -oov
            score = (
                m / n**power
                + lm * b
                + freq * u
                + length * n
                + language * w
                + geo * (rank == 0)
                + aosp * f
            )
            if score > best:
                best, result[i] = score, int(word)
    return result


def compare(before, after, sessions):
    rng = np.random.default_rng(42)
    groups = np.bincount(sessions)
    changes = np.bincount(sessions, after.astype(float) - before)
    draw = rng.integers(len(groups), size=(2000, len(groups)))
    return 100 * np.percentile(changes[draw].sum(1) / groups[draw].sum(1), [2.5, 97.5])


BASE = [0.3, 0.35, 0.0, 0.0, 1.0, 0, 0, 0.0, 0, 0.0]


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--inputs", nargs="+", required=True)
    p.add_argument("--sources", nargs="+", required=True)
    p.add_argument("--geo")
    p.add_argument("--frequency-folder")
    p.add_argument("--hits", help="Export frozen predictions for paired comparisons")
    p.add_argument("--initial", help="starting parameters for another validation round")
    p.add_argument("--fixed", help="comma-separated frozen parameters")
    args = p.parse_args()
    geo = {}
    if args.geo:
        with open(args.geo) as f:
            next(f)
            for line in f:
                key, _, *choices = line.rstrip("\n").split("\t")
                geo[int(key)] = choices
    rows, offsets, targets, sessions, base, iv = read(
        args.inputs, args.sources, geo, args.frequency_folder
    )

    def hit(params):
        return winners(rows, offsets, *params) == targets

    params = list(map(float, args.initial.split(","))) if args.initial else BASE.copy()
    en = hit([*params[:8], 1, params[9]])
    prev = hit(params)
    print(
        "rows sessions english-only bilingual steals rescues baseline",
        len(prev),
        len(set(sessions)),
        en.mean(),
        prev.mean(),
        (en & ~prev).sum(),
        (~en & prev).sum(),
        base.mean() if base.any() else "unavailable",
        flush=True,
    )
    if args.fixed:
        pars = list(map(float, args.fixed.split(",")))
        first = winners(rows, offsets, *pars)
        second = winners(rows, offsets, *pars, excluded=first[:, None])
        third = winners(rows, offsets, *pars, excluded=np.column_stack((first, second)))
        final = first == targets
        top3 = final | (second == targets) | (third == targets)
        monolingual = pars.copy()
        monolingual[8] = 1
        mono = hit(monolingual)
        print(
            "top3",
            top3.mean(),
            top3.sum(),
            "frozen English-only",
            mono.mean(),
            "steals",
            (mono & ~final).sum(),
            "rescues",
            (final & ~mono).sum(),
        )
        for label, group in (("IV", iv), ("OOV", ~iv)):
            print(
                label,
                "n",
                group.sum(),
                "top1",
                final[group].sum(),
                "top3",
                top3[group].sum(),
            )
        print("top1 95% CI", compare(np.zeros_like(final), final, sessions))
        print(
            "fixed top1 #",
            final.mean(),
            final.sum(),
            "delta CI",
            compare(prev, final, sessions),
            "paired vs geometry",
            compare(base, final, sessions) if base.any() else "unavailable",
            flush=True,
        )
        if args.hits:
            np.savez(
                args.hits,
                hit=final,
                sessions=sessions,
                baseline=base,
                reference=prev,
                top3=top3,
                mono=mono,
            )
        return
    for col, values in [
        (1, [0.35, 0.4, 0.45, 0.5, 0.55, 0.65, 0.85, 1, 1.3]),
        (0, [0, 0.15, 0.3, 0.375, 0.45, 0.5, 0.6]),
        (2, [0.1, 0.25, 0.5]),
        (3, [-0.5, -0.25, 0.25, 0.5, 1.0, 1.25, 1.5, 1.75, 2.2]),
        (4, [0.25, 0.5, 1.5, 2]),
        (
            9,
            [0.0, 0.0025, 0.005, 0.01, 0.0176, 0.025, 0.035, 0.05]
            if args.frequency_folder
            else [],
        ),
        (5, [1]),
        (6, [16.0, 20.0, 24.0, 28.0, 32.0, 40.0]),
        (0, [0, 0.1, 0.2, 0.3, 0.4, 0.5]),
        (1, [0.2, 0.35, 0.5, 0.65, 0.85, 1.0]),
        (3, [0.5, 1, 1.5, 2, 2.5]),
        (7, [0.25, 0.5, 1.0, 2.0] if geo else []),
    ]:
        if not values:
            continue
        candidates = []
        for value in values:
            test = params.copy()
            test[col] = value
            correct = hit(test)
            candidates.append((correct.sum(), value, correct))
        _, value, correct = max(candidates, key=lambda item: item[0])
        ci = compare(prev, correct, sessions)
        keep = ci[0] > 0
        print(
            col,
            value,
            100 * prev.mean(),
            100 * correct.mean(),
            ci,
            "keep" if keep else "reject",
            flush=True,
        )
        if keep:
            params[col], prev = value, correct
    print("parameters", params, flush=True)


if __name__ == "__main__":
    main()
