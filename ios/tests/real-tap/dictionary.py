"""Write an AOSP combined wordlist from the shipped SCLX probabilities."""

import math
import struct
import sys
from pathlib import Path

data = Path(sys.argv[1]).read_bytes()
magic, version, nodes, count, pairs, trie, ids, unigram, offsets, bows, links, chars = (
    struct.unpack_from("<4s11I", data)
)
assert magic == b"SCLX" and version == 1
alphabet = "abcdefghijklmnopqrstuvwxyzåäö'"
prefixes = [""] * nodes
words = [""] * count


def uint(offset):
    return struct.unpack_from("<I", data, offset)[0]


def frequency(cost):
    return max(0, min(255, round(255 - cost * 8.589237 / math.log(2))))


for node in range(nodes):
    bits = uint(trie + node * 8)
    if bits >> 29 & 1:
        words[uint(trie + node * 8 + 4) & 0xFFFFF] = prefixes[node]
    for child in range(bits & 0xFFFFFF, uint(trie + (node + 1) * 8) & 0xFFFFFF):
        letter = uint(trie + child * 8) >> 24 & 31
        prefixes[child] = prefixes[node] + alphabet[letter]
assert all(words)
print("dictionary=main:en,locale=en,description=SCLX,version=1,date=1")
for i, word in enumerate(words):
    print(f" word={word},f={frequency(data[unigram + i] / 10)}")
    for j in range(uint(offsets + 4 * i), uint(offsets + 4 * i + 4)):
        pair = uint(links + 4 * j)
        print(f"  bigram={words[pair & 0xFFFFF]},f={frequency((pair >> 20) / 128)}")
