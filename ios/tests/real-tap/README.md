# Replay recorded taps

Use macOS, Xcode, a disposable booted iOS simulator, Python with NumPy, and Java for dicttool. Run from the repo root. Set `D` to an absolute scratch directory and `UDID` to your test simulator. Keep corpora and donor assets outside the repository.

Clone the [Google TSI dataset](https://github.com/google-research-datasets/tap-typing-with-touch-sensing-images/tree/b7a6d7fb3d2ce9f8d5bb24ec492a720636da7bd4) into `$D/google`. Download [Vertanen and Kristensson's original IUI 2023 archive](https://zenodo.org/records/24225302/files/vertanen_noisy_typing_iui2023.zip) as `$D/word.zip`. Both datasets use CC BY 4.0.

```sh
for language in english bilingual; do
    ios/scripts/typing-sim.sh "$UDID" --languages "$language" --layout "$D/$language.json"
done
python3 ios/tests/real-tap/prepare.py "$D"
ios/scripts/typing-sim.sh "$UDID" --replay "$D/en.json" --out "$D/see-en.json"
ios/scripts/typing-sim.sh "$UDID" --replay "$D/en.json" --out "$D/free.json" --observed-spaces
ios/scripts/typing-sim.sh "$UDID" --languages bilingual --replay "$D/sv.json" --out "$D/see-sv.json"
```

Each JSON sentence has a participant, a stable sentence ID, intended words, one list of mapped taps per word, and the recorded boundary taps where available. Replay resets the text field between sentences and the adaptive model between people. The default run taps the space bar center after each recorded group. The observed-space run uses the recorded coordinate when present. Both flush the final word with space.

For the comparative run, supply the prior Mac LatinIME proof build of AOSP 127336e9 and an [iikb checkout at efa3c734](https://github.com/AVGVSTVS96/iikb/tree/efa3c73408737e3172eb39e4333e7933671c421b). Download sv.lexicon from the iikb language manifest into the replay directory and verify its SHA-256 against the manifest. The proof directory supplies the JNI-free headers, liblatinime.a, dicttool and the original dictionary assets. The runner script shows their paths.

```sh
ios/tests/real-tap/run-donors.sh "$D" "$AOSP_PROOF" "$IIKB_CHECKOUT"
python3 ios/tests/real-tap/score.py "$D"
python3 ios/tests/real-tap/detail.py "$D"
```

The inferred raw keyboard uses the nearest letter center inside each supplied word group. Word error uses edit distance on the final field. Recovery counts raw-wrong words present in the first three suggestions before space. The breakdown scores each commit when its space is typed. Later revisions and multiword edits can change final sentence distance. Intervals resample sentences with replacement. They describe these participants and conditions.
