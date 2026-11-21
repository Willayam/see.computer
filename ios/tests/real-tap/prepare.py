"""Prepare held-out replay JSON. Usage: prepare.py scratch (requires NumPy).
Inputs: google/ (TSI repo), word.zip (IUI 2023 logs), english.json and
bilingual.json (typing-sim.sh --layout). Outputs en.json and sv.json.
"""

import csv
import hashlib
import json
import random
import re
import sys
import zipfile
from collections import defaultdict
from pathlib import Path

import numpy as np

root = Path(sys.argv[1])
layout = json.loads((root / "english.json").read_text())


def rank(name):
    return int(hashlib.sha256(name.encode()).hexdigest(), 16)


def mapping(keys):
    letters = sorted(k for k in keys.keys() & layout.keys() if len(k) == 1)
    src = np.array([[*keys[k][:2], 1] for k in letters])
    dst = np.array([layout[k][:2] for k in letters])
    transform = np.linalg.lstsq(src, dst, rcond=None)[0]
    print("center RMS (pt)", np.sqrt(np.mean((src @ transform - dst) ** 2)))
    return lambda x, y: ([float(x), float(y), 1] @ transform).round(4).tolist()


def trial(row):
    return tuple(row[k] for k in ("participant_id", "task_id", "trial_id"))


def tsi():
    data = root / "google"
    keys = json.loads((data / "keyboard_data.json").read_text())["keys_info"]
    frames = {
        k: [v["key_center_x"], v["key_center_y"], v["key_width"], v["key_height"]]
        for k, v in keys.items()
    }
    project = mapping(frames)
    prompts = {
        trial(r): r["prompt"].rstrip(".")
        for r in csv.DictReader((data / "prompt_data.csv").open())
        if r["prompt_type"] == "phrase"
    }
    first = defaultdict(dict)
    wrong = defaultdict(list)
    for r in csv.DictReader((data / "touch_data.csv").open()):
        x, y = float(r["first_frame_touch_x"]), float(r["first_frame_touch_y"])

        def distance(k):
            cx, cy, w, h = frames[k]
            return (
                abs(x - cx) > w / 2 or abs(y - cy) > h / 2,
                (x - cx) ** 2 + (y - cy) ** 2,
            )

        wrong[r["participant_id"]].append(min(frames, key=distance) != r["ref_char"])
        key = trial(r)
        index = int(r["ref_char_index_in_prompt"])
        stamp = int(r["timestamp_ms"])
        if key in prompts and (index not in first[key] or stamp < first[key][index][0]):
            first[key][index] = (stamp, project(x, y))
    print(
        "all TSI on-key mean participant CER",
        np.mean([np.mean(v) for v in wrong.values()]),
    )
    for key, prompt in prompts.items():
        person = "tsi/" + key[0]
        if rank(person) % 2 == 0 or not re.fullmatch(
            r"[A-Za-z]+(?: [A-Za-z]+)*", prompt
        ):
            continue
        taps, spaces = [[]], []
        for i, c in enumerate(prompt):
            point = first[key].get(i, (None, None))[1]
            if c == " ":
                spaces.append(point)
                taps.append([])
            elif point is not None:
                taps[-1].append(point)
        yield dict(
            person=person,
            id="/".join(key),
            words=prompt.lower().split(),
            taps=taps,
            spaces=spaces + [None],
        )


def vertanen():
    with zipfile.ZipFile(root / "word.zip") as archive:
        for path in sorted(archive.namelist()):
            match = re.search(
                r"phone/(vt_exp2/test_word/[^/]+_space|vt_exp1/test_word/[^/]+_feed|eyesfree_exp1/test_word/[^/]+)\.log$",
                path,
            )
            if not match:
                continue
            exp, _, file = match[1].split("/")
            person = f"vertanen/{exp}/{file.split('_')[0]}"
            if rank(person) % 2 == 0:
                continue
            board = next(n for n in archive.namelist() if f"phone/{exp}/keyboard_" in n)
            frames = {}
            for line in archive.read(board).decode().splitlines():
                parts = line.split(";")
                if len(parts) == 5 and len(parts[0]) == 1:
                    frames[parts[0]] = list(map(float, parts[1:]))
            project = mapping(frames)
            sentences = []
            for i, block in enumerate(archive.read(path).decode().split("\n\n")):
                ref = re.search(r"^REF: (.+)$", block, re.M)
                inputs = re.findall(r"^IN: (.*)$", block, re.M)
                if (
                    not ref
                    or not re.fullmatch(r"[a-z]+(?: [a-z]+)*", ref[1])
                    or len(inputs) != len(ref[1].split())
                ):
                    continue
                taps = [
                    [
                        project(*tap.split(";")[0].split(",")[:2])
                        for tap in line.split("|")
                        if tap
                    ]
                    for line in inputs
                ]
                sentences.append(
                    dict(
                        person=person,
                        id=f"{path}/{i}",
                        words=ref[1].split(),
                        taps=taps,
                        spaces=[None] * len(taps),
                    )
                )
            yield from sorted(sentences, key=lambda s: rank(s["id"]))[:12]


def swedish():
    frames = json.loads((root / "bilingual.json").read_text())
    corpus = Path(__file__).parents[1] / "typing-eval/test-sv.txt"
    for i, line in enumerate(corpus.read_text().lower().splitlines()[:100]):
        words = re.findall(r"[a-zåäö]+", line)
        for level, spread in [("steady", 0.2), ("hurried", 0.3)]:
            rng = random.Random(f"{i}/{level}")

            def point(c):
                x, y = frames[c][:2]
                pitch = frames["q"][2]
                return [
                    x + rng.gauss(0, spread) * pitch,
                    y + rng.gauss(0, spread) * pitch,
                ]

            taps = []
            for word in words:
                coords = []
                for c in word:
                    slip = rng.random()
                    if slip >= 0.005:
                        coords.append(point(c))
                    if 0.005 <= slip < 0.01:
                        coords.append(point(c))
                taps.append(coords)
            yield dict(
                person="synthetic/" + level,
                id=f"{level}/{i}",
                words=words,
                taps=taps,
                spaces=[None] * len(words),
            )


for lang, trials in [("en", [*tsi(), *vertanen()]), ("sv", list(swedish()))]:
    trials.sort(key=lambda s: s["person"])
    (root / f"{lang}.json").write_text(json.dumps(trials))
