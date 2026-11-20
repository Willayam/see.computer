"""Rebuild offline keyboard word tables from Common Voice CC0 Sentence Collector text."""
import re
from collections import Counter, defaultdict
from pathlib import Path
from urllib.request import urlopen

REVISION = "1b139004ffcff2ad9f7907a984397730992f237c"
SOURCE = f"https://raw.githubusercontent.com/common-voice/common-voice/{REVISION}/server/data"
KEYBOARD = Path(__file__).resolve().parents[1] / "Keyboard"
WORD = re.compile(r"[^\W\d_]+(?:'[^\W\d_]+)*")


def word_table(sentences, language):
    counts = Counter()
    capitalized_inside = Counter()
    following = defaultdict(Counter)
    for sentence in sentences:
        previous = None
        end = 0
        for match in WORD.finditer(sentence.replace("’", "'")):
            word = match[0].lower()
            if not 1 <= len(word) <= 30:
                previous = None
                continue
            counts[word] += 1
            if previous and not match[0].islower() and not (language == "en" and word.split("'")[0] == "i"):
                capitalized_inside[word] += 1
            if previous and sentence[end:match.start()].isspace():
                following[previous][word] += 1
            previous, end = word, match.end()

    # Keep ordinary words. Proper names in these recordings are poor default suggestions.
    words = {word for word, count in counts.items() if count >= 3 and capitalized_inside[word] * 2 < count}
    rows = []
    for word in sorted(words, key=lambda w: (-counts[w], w)):
        successors = [w for w, n in following[word].most_common() if n >= 3 and w in words][:4]
        rows.append("\t".join([word, str(counts[word]), *successors]))
    return "\n".join(rows) + "\n"


if __name__ == "__main__":
    for locale, language in [("en", "en"), ("sv-SE", "sv")]:
        with urlopen(f"{SOURCE}/{locale}/sentence-collector.txt") as source:
            sentences = source.read().decode().splitlines()
        (KEYBOARD / f"words-{language}.tsv").write_text(word_table(sentences, language), encoding="utf-8")
