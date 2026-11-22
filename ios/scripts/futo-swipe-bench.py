"""Read keyboard lexicons and prepare encoder input for swipe-blend-bench."""

import re
import struct
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
        self.depths = np.array([len(w.replace("'", "")) for w in spelling], dtype=np.int32)
        # Follow apostrophe edges without consuming an emission.
        self.trailing = np.full(nodes, -1, dtype=int)
        edges = []
        for n, row in enumerate(self.children):
            choices = []
            for child in row:
                if child < 0:
                    break
                if self.letter[child] == ALPHABET.index("'"):
                    self.trailing[n] = self.terminal[child]
                    choices.extend(c for c in self.children[child] if c >= 0)
                else:
                    choices.append(child)
            edges.append(choices)
        self.children = np.full((nodes, max(map(len, edges))), -1, dtype=int)
        for n, row in enumerate(edges):
            self.children[n, :len(row)] = row
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


def features(points):
    x, y, t = np.array(points).T
    t = t - t[0]
    if t[-1] > 0.001:
        grid = np.linspace(0, t[-1], max(2, round(t[-1] * 0.06) + 1))
        x, y = np.interp(grid, t, x), np.interp(grid, t, y)
    index = np.arange(len(x))
    grid = np.linspace(0, len(x) - 1, 64)
    return torch.tensor(
        np.stack([np.interp(grid, index, x), np.interp(grid, index, y)]).clip(-0.1, 1.1)[None],
        dtype=torch.float32,
    )
