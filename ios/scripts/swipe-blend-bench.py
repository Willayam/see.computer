"""Export bilingual candidates for paired layout and scoring experiments."""

import argparse
import gzip
import importlib
import json
import math
import re
from pathlib import Path
from contextlib import nullcontext

import numpy as np
import torch
from swipe_ctc import beam, exact

bench = importlib.import_module("futo-swipe-bench")
ROWS = ("qwertyuiop", "asdfghjkl", "zxcvbnm")


def interpolate(x, knots, values):
    # Extend the outer segments to preserve off-key thumb offsets.
    i = np.searchsorted(knots, x).clip(1, len(knots) - 1)
    t = (x - knots[i - 1]) / (knots[i] - knots[i - 1])
    return values[i - 1] * (1 - t) + values[i] * t


def warp(points, source, dest):
    x, y, t = np.array(points).T
    src_y, dest_y, mapped = [], [], []
    for row in ROWS:
        src = np.array([source[c] for c in row])
        dst = np.array([dest[c] for c in row])
        src_y.append(src[0, 1])
        dest_y.append(dst[0, 1])
        mapped.append(interpolate(x, src[:, 0], dst[:, 0]))
    frac = np.interp(y, src_y, [0, 1, 2])
    i = frac.astype(int).clip(0, 1)
    mapped = np.asarray(mapped)
    j = np.arange(len(x))
    return np.column_stack(
        (
            mapped[i, j] * (1 - frac + i) + mapped[i + 1, j] * (frac - i),
            interpolate(y, np.array(src_y), np.array(dest_y)),
            t,
        )
    )


def tensors(layout):
    keys = torch.zeros(1, 64, 2)
    mask = torch.zeros(1, 64, dtype=torch.bool)
    for c, xy in layout.items():
        index = bench.ALPHABET.index(c)
        keys[0, index] = torch.tensor(xy)
        mask[0, index] = True
    return keys, mask


def context(before, lexicons):
    tail = re.split(r"[.!?\n]", before[-128:])[-1]
    history = re.findall(r"(?:[^\W\d_]|')+", tail.lower().replace("’", "'"))[-4:]
    odds = 0
    for word in history:
        freq = [
            math.exp(lex.uni[lex.ids[word]]) if word in lex.ids else 0
            for lex in lexicons
        ]
        odds = 0.7 * odds + math.log((freq[0] + 2e-6) / (freq[1] + 2e-6))
    p = 1 / (1 + math.exp(-max(-6, min(6, odds))))
    return " ".join(history), [math.log(p), math.log1p(-p)]


def main():
    parser = argparse.ArgumentParser()
    for name in ("model", "layout", "swedish-layout", "input", "out"):
        parser.add_argument("--" + name, required=True)
    parser.add_argument(
        "--geometry", choices=["native", "canonical"], default="canonical"
    )
    parser.add_argument(
        "--english-beam-only",
        action="store_true",
        help="Skip the Swedish beam on the canonical pass of a two-layout evaluation",
    )
    parser.add_argument("--replay-input", help="Write the mapped real gestures for TypingSim")
    parser.add_argument("--replay-layout", help="Write the corresponding normalized Swedish layout")
    parser.add_argument("--prune", action="store_true")
    parser.add_argument("--width", type=int, default=100)
    parser.add_argument("--uniform-time", action="store_true")
    parser.add_argument(
        "--geo", help="Include exact CTC scores for words proposed by TypingSim"
    )
    args = parser.parse_args()
    if args.width < 1:
        parser.error("--width must be positive")
    geometry = {}
    if args.geo:
        with open(args.geo) as f:
            next(f)
            for line in f:
                key, _, *words = line.rstrip("\n").split("\t")
                geometry[int(key)] = [w.lower() for w in words]
    torch.set_num_threads(1)
    encoder = bench.Runtime.get().load_program(args.model).load_method("forward")
    lexicons = [
        bench.Lexicon("ios/Keyboard/words-" + lang + ".bin") for lang in ("en", "sv")
    ]
    indices = [np.full(len(lex.children), -1, dtype=np.int32) for lex in lexicons]
    qwerty = {
        k["letter"]: (k["cx"], k["cy"])
        for k in json.loads(Path(args.layout).read_text())["keys"]
    }
    swedish = json.loads(Path(args.swedish_layout).read_text())
    if args.replay_layout:
        Path(args.replay_layout).write_text(json.dumps({"keys": [
            {"letter": letter, "cx": x, "cy": y} for letter, (x, y) in swedish.items()
        ]}))
    last_layout = None
    with (
        open(args.input) as source,
        (open(args.replay_input, "w") if args.replay_input else nullcontext()) as replay,
        gzip.open(args.out, "wt", compresslevel=1) as output,
    ):
        for line in source:
            r = json.loads(line)
            if "data" in r:
                points = [(p["x"], p["y"], p["t"]) for p in r["data"]]
                if points:
                    points = warp(points, qwerty, swedish)
                if replay:
                    mapped = [dict(p, x=x, y=y) for p, (x, y, _) in zip(r["data"], points, strict=True)]
                    print(json.dumps(dict(r, data=mapped)), file=replay)
                before = " ".join(r["sentence"].split()[: r["word_idx"]])
                layout = swedish
            else:
                if replay:
                    parser.error("--replay-input needs real gesture records")
                points, before, layout = r["points"], r["before"] or "", r["centers"]
            previous, weights = context(before, lexicons)
            word = re.sub(r"^[^\w]+|[^\w]+$", "", r["word"].lower().replace("’", "'"))
            session = r.get("session", r.get("sentenceID", r.get("id")))
            if not len(points):
                print(
                    json.dumps(
                        [word, session, weights, [[], []], r.get("baseline"), ""]
                    ),
                    file=output,
                )
                continue
            if args.geometry == "canonical":
                points = warp(points, layout, qwerty)
                layout = qwerty
            if args.uniform_time:
                points = np.array(points)
                points[:, 2] = np.arange(len(points)) * (1000 / 60)
            trace = bench.features(points)
            if layout != last_layout:
                keys = tensors(layout)
                last_layout = layout
            frames = encoder.execute((trace, *keys))[0][0].numpy()
            options = []
            for lang, (lex, index) in enumerate(zip(lexicons, indices)):
                if args.english_beam_only and lang == 1:
                    options.append([])
                    continue
                ids, mass = beam(
                    lex.children,
                    lex.letter,
                    lex.terminal,
                    lex.trailing,
                    frames,
                    args.width,
                    index,
                    lex.depths,
                    args.prune,
                )
                if args.geo:
                    additions = [
                        lex.ids[w]
                        for w in geometry[r["id"]]
                        if w in lex.ids and lex.ids[w] not in ids
                    ]
                    scores = [
                        exact(
                            np.array(
                                [
                                    bench.ALPHABET.index(c)
                                    for c in lex.words[i]
                                    if c != "'"
                                ],
                                dtype=np.int64,
                            ),
                            frames,
                        )
                        for i in additions
                    ]
                    ids = np.concatenate((ids, np.array(additions, dtype=np.int64)))
                    mass = np.concatenate((mass, scores))
                options.append(
                    list(
                        zip(
                            [lex.words[i] for i in ids],
                            mass.tolist(),
                            lex.prior(ids, previous).tolist(),
                            lex.uni[ids].tolist(),
                        )
                    )
                )
            path = frames.argmax(axis=1)
            greedy = "".join(
                bench.ALPHABET[c]
                for c in path[np.r_[True, path[1:] != path[:-1]]]
                if c < len(bench.ALPHABET)
            )
            output.write(
                json.dumps(
                    [
                        word,
                        session,
                        weights,
                        options,
                        r.get("baseline"),
                        greedy,
                        float(
                            exact(
                                np.array(
                                    [bench.ALPHABET.index(c) for c in greedy],
                                    dtype=np.int64,
                                ),
                                frames,
                            )
                        ),
                    ],
                    separators=(",", ":"),
                )
            )
            output.write("\n")


if __name__ == "__main__":
    main()
