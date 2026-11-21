"""Compare two typing-sim --out files sentence by sentence.

Prints each level's word error rate before and after, and a 95% paired bootstrap
interval for the change, so a keep-or-revert call rests on more than one number.
"""
import random
import sys
from collections import defaultdict


def load(path):
    rows = {}
    with open(path) as f:
        next(f)
        for line in f:
            level, language, index, words, errors = line.rstrip("\n").split("\t")
            rows[(level, language, int(index))] = (int(words), int(errors))
    return rows


def main(before_path, after_path):
    before, after = load(before_path), load(after_path)
    shared = sorted(before.keys() & after.keys())
    if len(shared) != len(before) or len(shared) != len(after):
        print(f"warning: {len(before)} vs {len(after)} sentences, comparing {len(shared)}")
    levels = defaultdict(list)
    for key in shared:
        levels[key[0]].append(key)
    levels["noisy"] = levels["steady"] + levels["hurried"]
    generator = random.Random(0)
    for level in ["clean", "steady", "hurried", "noisy"]:
        keys = levels[level]
        if not keys:
            continue
        words = sum(before[k][0] for k in keys)
        old = sum(before[k][1] for k in keys)
        new = sum(after[k][1] for k in keys)
        deltas = [after[k][1] - before[k][1] for k in keys]
        sizes = [before[k][0] for k in keys]
        samples = []
        for _ in range(2000):
            picks = [generator.randrange(len(keys)) for _ in keys]
            samples.append(sum(deltas[i] for i in picks) / sum(sizes[i] for i in picks) * 100)
        samples.sort()
        changed = sum(1 for d in deltas if d)
        print(f"{level:8} {old / words * 100:6.2f}% -> {new / words * 100:6.2f}%  "
              f"delta {(new - old) / words * 100:+.2f}pp  95% CI [{samples[50]:+.2f}, {samples[1949]:+.2f}]  "
              f"sentences changed {changed}")


if __name__ == "__main__":
    main(*sys.argv[1:3])
