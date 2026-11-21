"""Run the published encoder and a trie-constrained CTC prefix beam on replay traces.

Requires executorch, torch and numpy. Supply --synthetic for TypingSim's Swedish
JSONL export. Otherwise supply --layout and the sampled FUTO test JSONL.
"""

import argparse
import json
import re
import struct
import time
from pathlib import Path

import numpy as np
import torch
from executorch.runtime import Runtime

ALPHABET = "abcdefghijklmnopqrstuvwxyzåäö'"


class Lexicon:
    def __init__(self, path):
        data = Path(path).read_bytes()
        magic, version, nodes, self.start, _, *offset = struct.unpack_from(
            "<4s11I", data
        )
        assert magic == b"SCLX" and version == 1

        def array(section, dtype):
            return np.frombuffer(
                data[offset[section] : offset[section + 1]], dtype=dtype
            )

        bits, payload = array(0, "<u4").reshape(-1, 2).T
        self.letter = ((bits >> 24) & 31).astype(int)
        self.letter[0] = -1
        first = bits & 0xFFFFFF
        self.children = np.full((nodes, int(np.diff(first).max())), -1, dtype=int)
        self.terminal = (payload & 0xFFFFF).astype(np.int64)
        self.terminal[bits & (1 << 29) == 0] = -1
        spelling = [""]
        self.words = [""] * self.start
        for n in range(nodes):
            children = np.arange(first[n], first[n + 1], dtype=int)
            self.children[n, : len(children)] = children
            spelling.extend(spelling[n] + ALPHABET[self.letter[c]] for c in children)
            if self.terminal[n] >= 0:
                self.words[self.terminal[n]] = spelling[n]
        self.ids = {word: i for i, word in enumerate(self.words)}
        self.uni = -array(2, "B").astype(float) / 10
        self.rows = array(3, "<u4")
        self.backoff = -array(4, "B").astype(float) / 10
        self.pairs = array(5, "<u4")

    def prior(self, ids, before):
        words = re.findall(r"[\w']+|[.!?]", before.lower())
        previous = (
            self.start if not words or words[-1] in ".!?" else self.ids.get(words[-1])
        )
        if previous is None:
            return self.uni[ids]
        row = self.pairs[self.rows[previous] : self.rows[previous + 1]]
        prior = self.uni[ids] + self.backoff[previous]
        if len(row):
            keys = row & 0xFFFFF
            indices = np.minimum(np.searchsorted(keys, ids), len(row) - 1)
            hit = keys[indices] == ids
            prior[hit] = np.logaddexp(
                prior[hit], -(row[indices[hit]] >> 20).astype(float) / 128
            )
        return prior


def decode(lex, emissions, before):
    # Each active prefix stores separate masses for blank and nonblank endings.
    nodes = np.array([0])
    blank, letter = np.array([0.0]), np.array([-np.inf])
    for frame in emissions:
        total = np.logaddexp(blank, letter)
        edges = lex.children[nodes]
        parent, slot = np.nonzero(edges >= 0)
        child = edges[parent, slot]
        char = lex.letter[child]
        previous = lex.letter[nodes]
        extend = (
            np.where(char == previous[parent], blank[parent], total[parent])
            + frame[char]
        )
        new_nodes, index = np.unique(
            np.concatenate((nodes, child)), return_inverse=True
        )
        b = np.full(len(new_nodes), -np.inf)
        l = b.copy()
        keep = index[: len(nodes)]
        b[keep] = total + frame[64]
        l[keep] = np.where(nodes != 0, letter + frame[previous], -np.inf)
        np.logaddexp.at(l, index[len(nodes) :], extend)
        rank = np.logaddexp(b, l)
        chosen = (
            np.argpartition(rank, -100)[-100:]
            if len(rank) > 100
            else np.arange(len(rank))
        )
        nodes, blank, letter = new_nodes[chosen], b[chosen], l[chosen]

    ids = lex.terminal[nodes]
    valid = (ids >= 0) & np.isfinite(np.logaddexp(blank, letter))
    ids = ids[valid]
    scores = np.logaddexp(blank, letter)[valid]
    lengths = np.array([len(lex.words[i]) for i in ids])
    shape = scores / lengths**0.30

    def top(score):
        return [lex.words[ids[i]] for i in np.argsort(-score)[:3]]

    return top(shape), top(shape + 0.35 * lex.prior(ids, before))


def features(points):
    x, y, t = np.array(points).T
    t = t - t[0]
    if t[-1] > 0.001:
        grid = np.linspace(0, t[-1], max(2, round(t[-1] * 0.06) + 1))
        x, y = np.interp(grid, t, x), np.interp(grid, t, y)
    index = np.arange(len(x))
    grid = np.linspace(0, len(x) - 1, 64)
    return torch.tensor(
        np.stack([np.interp(grid, index, x), np.interp(grid, index, y)])[None],
        dtype=torch.float32,
    )


def main():
    parser = argparse.ArgumentParser()
    for name in ("model", "lexicon", "input", "out"):
        parser.add_argument("--" + name, required=True)
    parser.add_argument("--layout")
    parser.add_argument("--synthetic", action="store_true")
    args = parser.parse_args()
    torch.set_num_threads(1)
    encoder = Runtime.get().load_program(args.model).load_method("forward")
    lex = Lexicon(args.lexicon)
    if not args.synthetic:
        layout = {
            k["letter"]: (k["cx"], k["cy"])
            for k in json.loads(Path(args.layout).read_text())["keys"]
        }
    with open(args.input) as source, open(args.out, "w") as out:
        for i, line in enumerate(source):
            r = json.loads(line)
            if args.synthetic:
                layout, points, before = r["centers"], r["points"], r["before"]
            else:
                points = [(p["x"], p["y"], p["t"]) for p in r["data"]]
                before = " ".join(r["sentence"].split()[: r["word_idx"]])
            keys = torch.zeros(1, 64, 2)
            mask = torch.zeros(1, 64, dtype=torch.bool)
            for c, xy in layout.items():
                keys[0, ALPHABET.index(c)] = torch.tensor(xy)
                mask[0, ALPHABET.index(c)] = True
            # Sentence labels include punctuation entered separately from the swipe.
            word = re.sub(r"^[^\w]+|[^\w]+$", "", r["word"].lower().replace("’", "'"))
            start = time.perf_counter()
            if points:
                trace = features(points)
                emissions = encoder.execute((trace, keys, mask))[0][0].numpy()
                mid = time.perf_counter()
                raw, blend = decode(lex, emissions, before)
            else:
                mid = start
                raw, blend = [], []
            end = time.perf_counter()
            print(
                json.dumps(
                    {
                        "id": r.get("id", i),
                        "word": word,
                        "iv": word in lex.ids,
                        "raw": raw,
                        "blend": blend,
                        "baseline": r.get("baseline"),
                        "level": r.get("level"),
                        "encoder_ms": 1000 * (mid - start),
                        "beam_ms": 1000 * (end - mid),
                    }
                ),
                file=out,
            )


if __name__ == "__main__":
    main()
