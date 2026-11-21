"""Feed the continuous TSI stream to LatinIME by detecting boundaries in geometry."""

import json
import math
import sys
from pathlib import Path

root = Path(sys.argv[1])
layout = json.loads((root / "english.json").read_text())
bar = layout["Space"]
trials = []
for sentence in json.loads((root / "en.json").read_text()):
    if not sentence["person"].startswith("tsi/"):
        continue
    groups = [[]]
    for taps, space in zip(sentence["taps"], sentence["spaces"], strict=True):
        for point in taps + [space or bar[:2]]:

            def distance(key):
                x, y, w, h = layout[key]
                return (
                    abs(point[0] - x) > w / 2 or abs(point[1] - y) > h / 2,
                    math.dist(point, (x, y)),
                )

            nearest = min(layout, key=distance)
            if nearest == "Space":
                if groups[-1]:
                    groups.append([])
            else:
                groups[-1].append(point)
    trials.append(dict(sentence, taps=[g for g in groups if g]))
(root / "continuous.json").write_text(json.dumps(trials))
(root / "tsi.json").write_text(
    json.dumps(
        [
            s
            for s in json.loads((root / "en.json").read_text())
            if s["person"].startswith("tsi/")
        ]
    )
)
