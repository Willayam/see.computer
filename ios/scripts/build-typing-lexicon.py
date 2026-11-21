"""Build the memory-mapped keyboard trie and discounted language model."""

import argparse
import gzip
import hashlib
import math
import re
import struct
from collections import Counter, defaultdict
from pathlib import Path

from build_typing_sources import corpora, wordlist

ROOT = Path(__file__).resolve().parents[1]
ALPHABET = "abcdefghijklmnopqrstuvwxyzåäö'"
WORDS = re.compile(r"[^\W\d_]+(?:'[^\W\d_]+)*")


def normalized(sentence):
    return " ".join(re.findall(r"[^\W\d_]+", sentence.lower()))


def collect(language, cache):
    excluded = {
        normalized(s)
        for p in (ROOT / "tests/typing-eval").glob("*.txt")
        for s in p.read_text().splitlines()
    }
    uni, mid, capitals, pairs = (Counter() for _ in range(4))
    held = []
    alphabet = set(ALPHABET if language == "sv" else ALPHABET[:26] + "'")
    for cv, sentence in corpora(language, cache):
        key = normalized(sentence)
        if key in excluded:
            continue
        held_out = (
            cv
            and int.from_bytes(hashlib.sha1(key.encode()).digest()[:4], "little") % 20
            == 0
        )
        previous, end = "", 0
        weight = 3 if cv else 1
        for m in WORDS.finditer(sentence.replace("’", "'")):
            gap = sentence[end : m.start()]
            if gap.strip():
                previous = None
            word = m[0].lower()
            end = m.end()
            if len(word) > 24 or not alphabet.issuperset(word):
                previous = None
                continue
            if held_out:
                held.append((previous, word))
            else:
                uni[word] += weight
                if previous:
                    mid[word] += weight
                    if m[0].istitle():
                        capitals[word] += weight
                if previous is not None:
                    pairs[previous, word] += weight
            previous = word
    entries = {}
    with gzip.open(wordlist(language, cache), "rt") as source:
        for line in source:
            if line.startswith(" word="):
                fields = dict(
                    p.split("=", 1) for p in line.strip().split(",") if "=" in p
                )
                form = fields["word"].replace("’", "'")
                word, freq = form.lower(), int(fields["f"])
                if (
                    0 < len(word) <= 24
                    and alphabet.issuperset(word)
                    and freq > entries.get(word, (-1,))[0]
                ):
                    entries[word] = freq, form
    samples = [
        (f, math.log(uni[w])) for w, (f, _) in entries.items() if f and uni[w] >= 20
    ]
    x, y = (sum(col) / len(samples) for col in zip(*samples))
    slope = sum((a - x) * (b - y) for a, b in samples) / sum(
        (a - x) ** 2 for a, b in samples
    )
    vocab = sorted(
        {w for w, c in uni.items() if c >= 20}
        | {w for w, (f, _) in entries.items() if f or uni[w] >= 3}
    )
    prior = {
        w: max(0.05, 0.25 * min(50, math.exp(y + slope * (entries[w][0] - x))))
        if w in entries
        else 0.05
        for w in vocab
    }
    vocab_set = set(vocab)
    left, totals = Counter(), Counter()
    rows = defaultdict(dict)
    for (p, w), c in pairs.items():
        if (p == "" or p in vocab_set) and w in vocab_set:
            left[w] += 1
            totals[p] += c
            if c >= 2 or p == "":
                rows[p][w] = c
    n, k, z = sum(uni.values()), sum(left.values()), sum(prior.values())
    probs = {
        w: (0.5 * left[w] / k + 0.5 * uni[w] / n + 0.03 * prior[w] / z) / 1.03
        for w in vocab
    }
    z = sum(probs.values())
    probs = {w: p / z for w, p in probs.items()}
    proper = {
        w
        for w in vocab
        if (
            entries[w][1][0].isupper()
            if w in entries and mid[w] < 20
            else mid[w] >= 3 and capitals[w] > 0.7 * mid[w]
        )
        or (language == "en" and (w == "i" or w.startswith("i'")))
    }
    return probs, proper, rows, totals, held


def pack_model(language, model):
    probs, proper, rows, totals, held = model
    vocab = sorted(probs, key=lambda w: (-probs[w], w))
    ids = {w: i for i, w in enumerate(vocab)}
    chars, contexts = Counter(), Counter()
    trie = {}
    for word in vocab:
        node = trie
        for ch in word:
            node = node.setdefault(ch, {})
        node[""] = ids[word]
        sequence = "^^" + word + "$"
        for i in range(2, len(sequence)):
            chars[sequence[i - 2 : i + 1]] += 1
            contexts[sequence[i - 2 : i]] += 1
    nodes = [(trie, 0)]
    bits, terminals, masses, best = [], [], [], []
    word_nodes = [0] * len(vocab)
    for index, (node, letter) in enumerate(nodes):
        first = len(nodes)
        for ch, child in sorted(node.items()):
            if ch:
                nodes.append((child, ALPHABET.index(ch)))
        term = node.get("", None)
        p = probs[vocab[term]] if term is not None else 0
        flags = 0
        if term is not None:
            word_nodes[term] = index
            flags = 1 | (int(vocab[term] in proper) << 1)
        bits.append(first | letter << 24 | flags << 29)
        terminals.append(term or 0)
        masses.append(p)
        best.append(p)

    def quant(p, scale, limit):
        return min(limit, round(-math.log(max(p, 1e-30)) * scale))

    node_data = bytearray(8 * (len(nodes) + 1))
    bits.append(len(nodes))
    for i in reversed(range(len(nodes))):
        for child in range(bits[i] & 0xFFFFFF, bits[i + 1] & 0xFFFFFF):
            masses[i] += masses[child]
            best[i] = max(best[i], best[child])
        struct.pack_into(
            "<II",
            node_data,
            i * 8,
            bits[i],
            terminals[i] | quant(best[i], 2, 63) << 20 | quant(masses[i], 2, 63) << 26,
        )
    struct.pack_into("<I", node_data, len(nodes) * 8, len(nodes))
    offsets, backoff, pairs = [], bytearray(), bytearray()
    bows = {}
    for prev in vocab + [""]:
        offsets.append(len(pairs) // 4)
        row = rows.get(prev, {})
        bow = 1 - sum(c - 0.75 for c in row.values()) / max(1, totals[prev])
        bows[prev] = bow
        backoff.append(quant(bow, 10, 255))
        for idx, c in sorted((ids[w], c) for w, c in row.items()):
            pairs.extend(
                struct.pack(
                    "<I", idx | quant((c - 0.75) / totals[prev], 128, 4095) << 20
                )
            )
    offsets.append(len(pairs) // 4)

    def ints(values):
        return struct.pack(f"<{len(values)}I", *values)

    char_data = bytes(
        quant((chars[a + b + c] + 0.05) / (contexts[a + b] + 0.05 * 31), 10, 255)
        for a in ALPHABET + "^"
        for b in ALPHABET + "^"
        for c in ALPHABET + "$"
    )
    sections = [
        node_data,
        ints(word_nodes),
        bytes(quant(probs[w], 10, 255) for w in vocab),
        ints(offsets),
        backoff,
        pairs,
        char_data,
    ]
    starts = []
    data = bytearray(64)
    for section in sections:
        starts.append(len(data))
        data.extend(section)
    struct.pack_into(
        "<4s11I", data, 0, b"SCLX", 1, len(nodes), len(vocab), len(pairs) // 4, *starts
    )
    (ROOT / f"Keyboard/words-{language}.bin").write_bytes(data)
    loss = count = 0
    for prev, w in held:
        if w in probs:
            p = bows.get(prev, 1) * probs[w]
            c = rows.get(prev, {}).get(w)
            if c:
                p += (c - 0.75) / totals[prev]
            loss -= math.log(p)
            count += 1
    print(
        language,
        len(vocab),
        "words,",
        len(pairs) // 4,
        "pairs,",
        len(data),
        "bytes, held-out in-vocabulary perplexity",
        round(math.exp(loss / count), 2),
    )


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("cache", type=Path)
    parser.add_argument("languages", nargs="*", default=["en", "sv"])
    args = parser.parse_args()
    for language in args.languages:
        pack_model(language, collect(language, args.cache))
