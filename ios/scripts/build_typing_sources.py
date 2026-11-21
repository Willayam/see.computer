"""Text the keyboard's word tables learn from, shared by the table and eval builders."""
from urllib.request import urlopen

REVISION = "1b139004ffcff2ad9f7907a984397730992f237c"
SOURCE = f"https://raw.githubusercontent.com/common-voice/common-voice/{REVISION}/server/data"
LOCALES = {"en": "en", "sv": "sv-SE"}


def training_sentences(language):
    with urlopen(f"{SOURCE}/{LOCALES[language]}/sentence-collector.txt") as source:
        return source.read().decode().splitlines()
