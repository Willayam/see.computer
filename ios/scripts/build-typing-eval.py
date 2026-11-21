"""Sample held-out English and Swedish sentences from Tatoeba for the typing simulator.

Sentences the keyboard's word tables are built from are left out, so the eval never
types text the model has seen. The split is by hash, so a rerun on a newer export keeps
most of the same sentences.
"""
import bz2
import csv
import hashlib
import re
import sys
from pathlib import Path
from urllib.request import urlopen

sys.path.insert(0, str(Path(__file__).resolve().parent))
from build_typing_sources import training_sentences  # noqa: E402

TATOEBA = "https://downloads.tatoeba.org/exports/per_language/{0}/{0}_sentences.tsv.bz2"
OUT = Path(__file__).resolve().parents[1] / "tests" / "typing-eval"
ALLOWED = re.compile(r"^[A-Za-zÅÄÖåäö][A-Za-zÅÄÖåäö ,.?!']*$")
PER_SET = 800


def usable(sentence):
    words = sentence.split()
    return ALLOWED.match(sentence) and 4 <= len(words) <= 18 and "  " not in sentence


def normalized(sentence):
    return " ".join(re.findall(r"[^\W\d_]+", sentence.lower()))


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    for language, code in [("en", "eng"), ("sv", "swe")]:
        seen = {normalized(s) for s in training_sentences(language)}
        with urlopen(TATOEBA.format(code)) as response:
            rows = bz2.decompress(response.read()).decode().splitlines()
        sentences = []
        keys = set()
        for row in csv.reader(rows, delimiter="\t", quoting=csv.QUOTE_NONE):
            text = row[2].strip().replace("’", "'")
            key = normalized(text)
            if usable(text) and key not in seen and key not in keys:
                keys.add(key)
                sentences.append(text)
        sentences.sort(key=lambda s: hashlib.sha1(s.encode()).hexdigest())
        for index, name in enumerate(["dev", "test"]):
            chosen = sentences[index * PER_SET:(index + 1) * PER_SET]
            (OUT / f"{name}-{language}.txt").write_text("\n".join(chosen) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
