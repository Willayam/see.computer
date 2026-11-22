"""Select a repeatable uniform sample of FUTO JSONL records in file order."""

import argparse
import random
import sys

parser = argparse.ArgumentParser()
parser.add_argument("input")
parser.add_argument("--count", type=int, default=5000)
parser.add_argument("--seed", type=int, default=42)
args = parser.parse_args()
with open(args.input, "rb") as source:
    selected = set(
        random.Random(args.seed).sample(range(sum(1 for _ in source)), args.count)
    )
    source.seek(0)
    for index, record in enumerate(source):
        if index in selected:
            sys.stdout.buffer.write(record)
