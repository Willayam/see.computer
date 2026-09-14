"""Write per-clip latency statistics and transcript variants from raw JSONL."""
import collections
import csv
import json
import math
from pathlib import Path
import statistics
import sys

source = Path(sys.argv[1])
rows = [json.loads(line) for line in source.read_text().splitlines()]
groups = collections.defaultdict(list)
texts = collections.defaultdict(set)
for row in rows:
    if row["event"] == "sample":
        groups[row["mode"], row["condition"], row["clip"]].append(row["ms"])
        texts[row["mode"], row["clip"]].add(row["text"])
with source.with_name("summary.csv").open("w") as file:
    writer = csv.writer(file)
    writer.writerow(["backend", "condition", "clip", "n", "median_ms", "p95_ms", "min_ms", "max_ms"])
    for key, values in sorted(groups.items()):
        values.sort()
        stats = [statistics.median(values), values[math.ceil(len(values) * .95) - 1], values[0], values[-1]]
        writer.writerow([*key, len(values), *[round(value, 3) for value in stats]])
source.with_name("transcripts.json").write_text(json.dumps([
    {"backend": mode, "clip": clip, "variants": sorted(variants)}
    for (mode, clip), variants in sorted(texts.items())
], ensure_ascii=False, indent=2) + "\n")
print(source.with_name("summary.csv").read_text())
