"""Sources shared by the keyboard lexicon and eval builders."""

import base64
import bz2
import csv
import os
import re
import tarfile
from collections import Counter
from itertools import islice
from pathlib import Path
from shutil import copyfileobj
from tempfile import NamedTemporaryFile
from urllib.request import urlopen

REVISION = "1b139004ffcff2ad9f7907a984397730992f237c"
SOURCE = f"https://raw.githubusercontent.com/common-voice/common-voice/{REVISION}/server/data"
LOCALES = {"en": "en", "sv": "sv-SE"}
LEIPZIG = {
    "en": ("eng_news_2023_1M", "eng-com_web-public_2018_1M"),
    "sv": ("swe_news_2023_1M", "swe_news_2022_1M"),
}
AOSP = {"en": "en_US_wordlist.combined.gz", "sv": "sv_wordlist.combined.gz"}
TATOEBA = "https://downloads.tatoeba.org/exports/per_language/{0}/{0}_sentences.tsv.bz2"
TATOEBA_CODES = {"en": "eng", "sv": "swe"}
WORD = re.compile(r"[^\W\d_]+")


def cached(cache, name, url, encoded=False):
    cache.mkdir(parents=True, exist_ok=True)
    path = cache / name
    if not path.exists():
        with NamedTemporaryFile(dir=cache, delete=False) as tmp:
            try:
                with urlopen(url) as response:
                    if encoded:
                        tmp.write(base64.b64decode(response.read()))
                    else:
                        copyfileobj(response, tmp)
                os.replace(tmp.name, path)
            finally:
                Path(tmp.name).unlink(missing_ok=True)
    return path


def wordlist(language, cache):
    name = AOSP[language]
    return cached(
        cache,
        "aosp_" + name,
        f"https://android.googlesource.com/platform/packages/inputmethods/LatinIME/+/refs/heads/main/dictionaries/{name}?format=TEXT",
        True,
    )


def corpora(language, cache):
    for name in LEIPZIG[language]:
        path = cached(
            cache,
            name + ".tar.gz",
            f"https://downloads.wortschatz-leipzig.de/corpora/{name}.tar.gz",
        )
        with tarfile.open(path, "r|gz") as archive:
            member = next(m for m in archive if m.name.endswith("-sentences.txt"))
            for line in islice(archive.extractfile(member), 500_000):
                yield False, line.decode().rstrip().split("\t", 1)[1]
    locale = LOCALES[language]
    path = cached(
        cache, f"cv-{locale}.txt", f"{SOURCE}/{locale}/sentence-collector.txt"
    )
    with path.open() as lines:
        for line in lines:
            yield True, line.strip()


def everyday(language, cache, capitals, size):
    """Tatoeba's short everyday sentences, the closest of the sources to typed messages.

    Its contributors return to a few names and places, Tom, Rima and Algeria among them, far
    more than other writers do. A sentence is left out when it holds a capitalized word used
    ten times as often here as in the other corpora, where `capitals` counts each such word
    among `size` words.
    """
    code = TATOEBA_CODES[language]
    path = cached(cache, f"tatoeba-{code}.tsv.bz2", TATOEBA.format(code))
    rows = bz2.decompress(path.read_bytes()).decode().splitlines()
    texts = [row[2].strip() for row in csv.reader(rows, delimiter="\t", quoting=csv.QUOTE_NONE)]
    names = [[w.lower() for w in WORD.findall(text) if w.istitle()] for text in texts]
    used = Counter(name for found in names for name in found[1:])
    total = sum(len(WORD.findall(text)) for text in texts)
    favored = {
        name
        for name, count in used.items()
        if count >= 20 and count / total > 10 * (capitals[name] + 1) / size
    }
    for text, found in zip(texts, names):
        if favored.isdisjoint(found):
            yield text


def training_sentences(language):
    cache = Path(os.environ.get("TYPING_CACHE", Path.home() / ".cache/see-typing"))
    return (sentence for _, sentence in corpora(language, cache))
