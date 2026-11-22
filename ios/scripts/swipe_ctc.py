"""Sparse CTC prefix search and exact single-spelling CTC scoring."""

import numpy as np
from numba import njit


@njit(cache=True)
def beam(children, letters, terminal, trailing, frames, width, index, depths, prune):
    nodes = np.zeros(width, np.int64)
    blanks = np.full(width, -np.inf)
    nonblanks = blanks.copy()
    blanks[0] = 0.0
    count = 1
    size = width * (1 + children.shape[1])
    ids = np.empty(size, np.int64)
    b = np.empty(size)
    l = np.empty(size)
    for frame in frames:
        used = count
        for i in range(count):
            n = nodes[i]
            index[n] = i
            ids[i] = n
            b[i] = np.logaddexp(blanks[i], nonblanks[i]) + frame[64]
            l[i] = nonblanks[i] + frame[letters[n]] if n else -np.inf
        for i in range(count):
            n = nodes[i]
            total = np.logaddexp(blanks[i], nonblanks[i])
            for child in children[n]:
                if child < 0:
                    break
                c = letters[child]
                mass = (blanks[i] if c == letters[n] else total) + frame[c]
                j = index[child]
                if j < 0:
                    j = used
                    used += 1
                    index[child] = j
                    ids[j] = child
                    b[j] = -np.inf
                    l[j] = mass
                else:
                    l[j] = np.logaddexp(l[j], mass)
        rank = np.logaddexp(b[:used], l[:used])
        if prune:
            for j in range(used):
                length = max(1, depths[ids[j]])
                rank[j] = rank[j] / length**0.4234 + 1.0382 * length
        count = min(width, used)
        keep = np.argpartition(rank, used - count)[-count:]
        for i, j in enumerate(keep):
            nodes[i], blanks[i], nonblanks[i] = ids[j], b[j], l[j]
        for j in range(used):
            index[ids[j]] = -1
    mass = np.logaddexp(blanks[:count], nonblanks[:count])
    words = np.concatenate((terminal[nodes[:count]], trailing[nodes[:count]]))
    mass = np.concatenate((mass, mass))
    valid = (words >= 0) & np.isfinite(mass)
    return words[valid], mass[valid]


@njit(cache=True)
def exact(codes, frames):
    states = np.full(2 * len(codes) + 1, 64)
    states[1::2] = codes
    prev = np.full(len(states), -np.inf)
    prev[0] = 0.0
    for frame in frames:
        for j in range(len(states) - 1, -1, -1):
            mass = prev[j]
            if j:
                mass = np.logaddexp(mass, prev[j - 1])
            if j > 1 and states[j] != 64 and states[j] != states[j - 2]:
                mass = np.logaddexp(mass, prev[j - 2])
            prev[j] = mass + frame[states[j]]
    return np.logaddexp(prev[-1], prev[-2]) if len(codes) else prev[0]
