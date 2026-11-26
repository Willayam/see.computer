"""Sample four consecutive subtitle lines per field. Keep downloads outside the repo."""
import hashlib
import re
import sys
import xml.etree.ElementTree as ET
import zipfile
from pathlib import Path

from build_typing_sources import cached

DIGEST = {
    "en": "bbc015e9c914afb38b1a0645aef68ef6faedaefebf873d1e13152d67aedf6756",
    "sv": "a36afecbf4aa4c4dc4933370645c36f33ac99105f8df3ced801c607625a3f07c",
}


def digest(text):
    return hashlib.sha256(text.encode()).hexdigest()


def sample(directory, language):
    archive = cached(directory, language + ".zip",
                     f"https://object.pouta.csc.fi/OPUS-OpenSubtitles/v1/raw/{language}.zip")
    if hashlib.sha256(archive.read_bytes()).hexdigest() != DIGEST[language]:
        raise ValueError(f"Archive changed: {archive}")
    alphabet = "a-zA-Z" + ("ÅÄÖåäö" if language == "sv" else "")
    entries = {"dev": [], "test": []}
    movies = set()
    with zipfile.ZipFile(archive) as subtitles:
        for name in sorted(subtitles.namelist(), key=digest):
            if not name.endswith(".xml"):
                continue
            movie = Path(name).stem.split("_")[0]
            if movie in movies:
                continue
            movies.add(movie)
            split = "test" if int(digest(movie), 16) % 2 else "dev"
            lines = [re.sub(r"(?<=\w)' +(?=\w)", "'", (s.text or "").strip(" -"))
                     for s in ET.fromstring(subtitles.read(name)).iter("s")]
            choices = []
            for i in range(0, len(lines) - 3, 4):
                group = lines[i:i + 4]
                text = " ".join(group)
                if (all(re.fullmatch(f"[{alphabet}][{alphabet} ,.?!']*", line) for line in group)
                        and 12 <= len(text.split()) <= 40):
                    choices.append(text)
            entries[split].extend(sorted(choices, key=digest)[:6])
    for split, pool in entries.items():
        chosen = sorted(set(pool), key=digest)[:300]
        if len(chosen) != 300:
            raise ValueError(f"Only {len(chosen)} entries for {language} {split}")
        path = directory / f"{split}-{language}.txt"
        path.write_text("\n".join(chosen) + "\n", encoding="utf-8")
        tokens = [re.findall(r"[a-zåäö]+(?:'[a-zåäö]+)*", line.lower()) for line in chosen]
        total = sum(map(len, tokens))
        repeats = total - sum(len(set(words)) for words in tokens)
        print(f"{path.name} entries={len(chosen)} repeats={repeats}/{total} ({repeats/total:.2%})")


if __name__ == "__main__":
    for lang in DIGEST:
        sample(Path(sys.argv[1]), lang)
