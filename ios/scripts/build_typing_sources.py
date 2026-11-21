"""Sources shared by the keyboard lexicon and eval builders."""

import base64
import os
import tarfile
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


def training_sentences(language):
    cache = Path(os.environ.get("TYPING_CACHE", Path.home() / ".cache/see-typing"))
    return (sentence for _, sentence in corpora(language, cache))
