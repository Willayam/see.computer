"""Average native and canonical English CTC masses. Retain the native Swedish beam."""

import argparse
import gzip
import json
import math

parser = argparse.ArgumentParser()
for name in ("native", "canonical", "out"):
    parser.add_argument("--" + name, required=True)
parser.add_argument("--canonical-weight", type=float, default=0.5)
args = parser.parse_args()
if not 0 <= args.canonical_weight <= 1:
    parser.error("canonical weight must be in [0, 1]")
with (
    gzip.open(args.native) as native,
    gzip.open(args.canonical) as canonical,
    gzip.open(args.out, "wt", compresslevel=1) as output,
):
    for raw, mapped in zip(native, canonical, strict=True):
        row, alternate = json.loads(raw), json.loads(mapped)
        assert row[:3] == alternate[:3]
        candidates = {}
        for weight, beam in (
            (1 - args.canonical_weight, row[3][0]),
            (args.canonical_weight, alternate[3][0]),
        ):
            for word, mass, prior, uni in beam:
                if word not in candidates:
                    candidates[word] = [word, 0.0, prior, uni]
                candidates[word][1] += weight * math.exp(mass)
        row[3][0] = [
            [word, math.log(mass), prior, uni]
            for word, mass, prior, uni in candidates.values()
            if mass
        ]
        print(json.dumps(row, separators=(",", ":")), file=output)
