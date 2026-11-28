# see.computer for iPhone

A voice keyboard for iOS. It runs the Mac app's speech engine and text passes on the phone, so dictation stays on the device and reads the same dictionary. You can install it directly from Xcode or distribute the store build through TestFlight.

## Install

You need a Mac with Xcode, Rust, and an Apple account on a paid developer team. Sign in under Xcode > Settings > Accounts, and change `DEVELOPMENT_TEAM` in the project if the team is not yours.

```sh
rustup target add aarch64-apple-ios aarch64-apple-ios-sim
ios/scripts/install-phone.sh
```

On the phone:

1. Open see.computer. It downloads the 670 MB speech model on first launch. Keep the screen open until it finishes.
2. In Settings, go to General > Keyboard > Keyboards > Add New Keyboard and choose see.computer.
3. Tap see.computer in that list and turn on Allow Full Access. The keyboard needs it to reach the app. It uses no network.
4. In any text field, hold the globe key and choose see.computer.

To use your Mac dictionary, tap **Use the dictionary from your Mac** in the app and choose iCloud Drive > Documents > see.computer. The app copies `vocabulary.md` and `learned.md` each time it comes forward. The phone only reads them, so corrections made on the phone do not teach it anything yet.

## TestFlight

`ios/scripts/testflight.sh` archives a build and uploads it to App Store Connect. Create the app there first, with the bundle ID `computer.see.ios`, because only the website can do that. In this build, open see.computer and turn the mic on before dictating. Switch back to the app you were typing in and tap **Dictate**. While the mic stays ready, you can start and finish later takes from the keyboard. Dictate starts nothing while the speech model is still loading or downloading, and the keyboard says which.

## Using it

Tap **Dictate** above the letters, talk, then tap **Done** in the same spot. Or hold Dictate, talk, and let go. You can type, delete and move the cursor while the mic keeps recording. The words already in the field stay there as ordinary text, and the words you speak after that show in the top row. Done puts only those later words at the cursor, wherever you left it in the same field. If the field, selection, or surrounding text changes outside the keyboard after you tap Done, the keyboard types nothing. It shows **Insert** with the words in the top row instead. Tap it to put them at the cursor, or tap the X to leave them in History. The offer stays for 10 minutes, in whichever field the keyboard shows next. Typing hides it until the keyboard next comes up. Sending the message ends the take. The X cancels. When the transcript lands, select a word in the host app and type its replacement on the same keyboard. Tap Dictate again to continue speaking.

**Typing languages** in the see.computer app follows the iPhone’s preferred languages by default. Choose **English & Svenska** once to keep Å, Ä and Ö on the letter rows in every app. Shift changes case, a double tap locks capitals, and **123** opens numbers and symbols. Hold and slide the space bar to position the cursor. Double-tap a word in the host app to use its Copy and Paste menu. Double-tap space after a letter or digit to insert a period and a space. Typing a space after a number returns to the letters.

The keyboard keeps where each finger landed. While you type, a tap near the edge of a key goes to the letter that more likely comes next, and a tap in the middle of a key always types that key. At a space or punctuation it reads the whole word again from the touch points, the words around it and English and Swedish word frequencies. That fixes neighbor keys, skipped, doubled and swapped letters, a space that landed on a letter, and a letter that landed on the space bar. It can also fix the word before. The suggestion on a pale capsule is what space or punctuation will accept. Tap the quoted spelling to keep it, or press backspace immediately after a correction to restore what you typed. Tap a suggestion to finish a word or insert the next one. The suggestions follow the two words before the cursor, and one that is too long for its third of the row shrinks to fit. In bilingual mode the recent words help rank suggestions, and a word from either language can stay. The keyboard also uses the iPhone’s text replacements and the names iOS shares with it.

The keyboard scores nearby keys using English and Swedish word frequencies from the AOSP dictionaries, Leipzig news and web corpora, Mozilla Common Voice, and Tatoeba's everyday sentences. The bundled models run on device. Rebuild them with `python3 ios/scripts/build-typing-lexicon.py /path/to/cache`. [Data sources and licenses](THIRD_PARTY_NOTICES.md)

The words appear in the text field as you talk, underlined until the take is done, and the final text replaces them. If a field stops holding them, because the cursor moved, the rest show on the keyboard. Some apps show no underlined text. There the words show on the keyboard while you talk, and the whole take goes into the field when it is done.

Leaving the keyboard does not end a take. The Dynamic Island and the Lock Screen show the mic while it is on, with a clock and the mic level during a take. **Done** there finishes the take, and **Turn off mic** turns the mic off between takes. A take ends by itself after 5 minutes away from the keyboard. Its text replaces the words in its own field if you come back within 10 minutes, even when you dictated another take in another app in between. When too little of those words is left before the cursor to be sure of the place, the keyboard changes nothing and offers the missing words with **Insert**. Sending a message while the words are still underlined also ends the take, and the final text is not typed again. If the app dies mid-take, the words stay in the field as plain text. Every finished take is in History, whether or not it reached a field.

iOS gives keyboards no microphone, so the app has to hold it. In a direct Xcode install, when the app is not already listening, the first tap opens it, turns the mic on, and returns to the host app if iOS allows it. Dictation starts when you land. After that the mic stays on in the background, with the orange dot showing, and later taps never leave the app you are in. **Keep the mic ready for** sets how long. A phone call turns it off, and the next tap brings it back the same way.

While armed, the app holds the last 300 ms of audio and discards the rest. Nothing is kept until you tap.

Dictations are saved as one markdown file per day in the app's folder, visible in the Files app, in the same format as the Mac's history. Tap one in the app to copy it.

The keyboard hands over to the next keyboard in number, phone and email fields, and iOS replaces it in password fields.

## How it is built

The app owns the microphone, the engine and `state.json` in the shared App Group folder. `state.json` lists every finished take the keyboard may still have to place, each for 10 minutes. The keyboard reads that file and sends Darwin notifications (`start`, `stop`, `cancel`, `ping`, `away`, `sent`). It holds no audio. It keeps `ledger.json`, one entry per take: the words it put in the field while the take ran, the user's own edit, and whether the finished take was sent to the host, placed, offered or dismissed. The ledger says a take was sent before the host hears of it, so a take is typed once, however often iOS replaces the keyboard. The keyboard decides whether the app is alive by whether a `ping` gets a `pong`, because a killed app cannot correct the file it left behind. The `Island` widget extension draws the Live Activity, and only the app starts and updates it.

Before it changes the host field, the keyboard checks the field identifier and fingerprints of the surrounding text. It writes the words so far with `setMarkedText`, the call keyboards for Japanese use for text they have not committed. Marked text is the one edit a host answers. A host that ignores it sends the field back unchanged, within 50 ms in the hosts measured. The keyboard then shows the words on its own row and types the finished take as plain text. A host says nothing about a plain insert, so the keyboard cannot tell when one was dropped.

A host reports only the sentence or two before the cursor, at most 300 characters. iOS gives a field a new identifier each time the keyboard comes back to it, and it can leave the take's words marked, commit them or drop them. So a take that ended while the keyboard was away finds its field by the words it left there. The keyboard first commits a mark it did not set, in a turn of its own, because one delete removes a whole marked run.

`core/` is a small Rust crate that compiles the Mac's own source files by path: `engine/`, `boost.rs`, `vocabulary.rs`, `filler.rs`, `numbers.rs`, `text.rs`, `align.rs` and `qos.rs`. `core/src/seams.rs` stands in for the few Mac modules they reach into. A change to any of those files reaches both apps. CI builds this crate so a Mac change cannot break it unnoticed.

The keys copy Apple's keyboard as measured in the simulator, down to SF Compact for the key labels. UIKit does not offer that face, so the keyboard loads it with Core Text from the folder that holds the system font, and falls back to SF Pro if the file is not there. iOS draws 17 pt of keyboard above any custom keyboard and ends it above its own globe row, so this keyboard stands 16 pt taller than Apple's. Every key keeps Apple's size and gaps, and the four rows sit 4 pt higher than Apple's so the bottom row is not cut short.

The engine is Parakeet TDT 0.6b v3 (INT8, ONNX) on the CPU, as on the Mac. The Neural Engine would use less power, but [Sayboard](https://github.com/stanlsv/sayboard), an open source voice keyboard, treats it as blocked for background apps from iOS 27 and falls back to the CPU. The mapped weights stay out of the app's memory footprint, so it sits near 135 MB while loaded.

## What relies on undocumented iOS behavior

Direct installs use these shortcuts. The store build excludes them.

- `HostCapture` reads the host app's bundle id from a private UIKit callback, and `HostReturn` opens that app with a private system class. iOS 26.4 removed every supported way to do this. When either fails, the app shows a screen asking you to swipe right along the bottom edge.
- The keyboard opens the app by walking its responder chain to the `UIApplication` object. Apple documents opening the containing app from a keyboard as unsupported. When it fails, the keyboard asks you to open the app yourself.

## Diagnostics log

The app and the keyboard both append one line per event to `Library/diagnostics.tsv` in the shared App Group folder: every step of a take, every way one ends, and every tap the keyboard could not act on. Each line holds the time in unix milliseconds, `app` or `keyboard`, the event name, and `key=value` pairs. It records lengths, durations, levels and error messages, never the dictated words. The file empties itself once it passes 1 MiB.

To copy it off a connected iPhone:

```sh
ios/scripts/pull-diagnostics.sh [destination] [device id]
```

The destination defaults to `./diagnostics-iphone.tsv`. The log sits under `Library` because `devicectl` will not copy files from the top of an App Group folder.

## Verify it without a microphone

Debug builds replace the microphone when `Documents/test-input.wav` exists in the app's container. Every take then returns that file's audio, as `SEE_COMPUTER_AUDIO_FILE` does on the Mac. Use this in the simulator, because the input node goes through the Mac's audio service there and aborts the app if that service does not answer in time. The file microphone plays silence, so the app keeps running behind the host app as it does with a real microphone.

`scripts/test-draft.sh` checks the rules for where a finished take may be typed. `scripts/test-engine.sh` runs `Engine.swift` and `CaptureGate.swift` against a stand-in for the Rust worker and checks the order in which audio reaches it. Both run on the Mac without a simulator.

`scripts/delivery-host.sh` dictates into real text fields in a booted simulator and checks what each field ends up holding. Its host app has a field that ignores marked text and one that ignores every edit. The scenarios cover a key typed and a message sent mid-take, a take that ends while the keyboard is away, a second take dictated meanwhile, a keyboard process killed before the host answered, and the files an older build left behind. The header of the script lists what it needs.

`scripts/bench-phone.sh <device id> <wav>` transcribes the file repeatedly on a connected iPhone and prints one JSON line per run with the time, the memory footprint, and whether the app was in the foreground. It needs a debug build and a model already on the phone.

## Measure typing accuracy

`scripts/typing-sim.sh <simulator id> --out run.tsv` lays out the real keys in a booted simulator and types 1,600 held-out English and Swedish sentences three times: on key centers, with a touch spread of 0.2 key widths, and with 0.3. It adds a skipped, doubled or swapped letter at 0.5% each per letter. It prints the word error rate per level and `noisy_wer`, the rate over the two noisy levels. `python3 scripts/typing-sim-compare.py before.tsv after.tsv` compares two runs sentence by sentence with a 95% interval. Tune on `--set dev` and confirm on `--set test`. One run takes about three minutes.

`scripts/typing-sim.sh <simulator id> --mode suggest` scores the suggestion row on the same sentences, typed without mistakes. `keystrokes_saved` is the share of taps a typist skips by tapping the intended word as soon as the row offers it, and `next_word_top3` is how often the row offers a word before its first letter. The spelling typed so far does not count as an offer. Add `--input sentences.txt` to score other text, one sentence per line.

Swipe across letter keys and lift to insert a word. The keyboard supplies the space before the next swiped word. Tap an alternative in the suggestion row to replace the word, or press delete immediately to remove it.

Use `--mode swipe` with the same simulator command and corpus. The generator collapses adjacent repeated letters, omits internal apostrophes from the path and taps standalone punctuation, digits and single letters. The curved generator slows at direction changes over 37 degrees and at word boundaries. Each leg bows sideways by a Gaussian offset that peaks halfway along the chord. Its standard deviation is 3% of leg length for steady strokes and 6% for hurried strokes. Target noise scales with key pitch (0.20 steady, 0.35 hurried), with multipliers of 1.2 along the incoming direction and 0.75 across it. Endpoints also vary along the approach. A moving average rounds noisy corners. Clean strokes pass through key centres on straight legs. Use `--gesture straight` for the original isotropic generator with a slowdown at every target.

The use of noisy via points follows [Modeling Gesture-Typing Movements](https://research.google/pubs/modeling-gesture-typing-movements/). [How We Swipe](https://research.aalto.fi/en/publications/how-we-swipe-a-large-scale-shape-writing-dataset-and-empirical-fi/) reports a mean swipe duration of 689 ms and mean trajectory length of 371 px for its English Enron phrases. The simulator chooses a nominal speed of 10 to 20 key pitches per second and samples at 60 to 120 Hz. The noise scales, bow and corner threshold here are synthetic stress settings. They have not been fitted to those traces or to Swedish input.

WER counts substitutions, insertions and omissions in the resulting field, including missed gesture starts. Decode timing covers successful gesture recognition through candidate ranking, excluding drawing and host text edits. The ten-letter timing groups by reference word length. Run `--mode swipe --endpoints` to score a control that reads only the first and last keys. This synthetic benchmark does not measure actual finger trajectories, correlated drift or host-app touch delivery.

### Swipe typing

The keyboard loads the FUTO Swipe FP32 encoder on the first English or bilingual
swipe. On the Swedish layout, English candidates combine native and QWERTY
encoder passes. The geometric decoder supplies Swedish words and their order.
The native Swedish beam provides evidence when the decoders disagree about the language. Turning off
**New swipe model** in the app's Keyboard section switches English back to geometry.

After swiping in a device build with Full Access, run
`ios/scripts/swipe-budget.sh` on the connected Mac. It reports cold load time,
latency and peak keyboard footprint from the first and every twentieth subsequent neural gesture.
For English encoder parity on a booted simulator, run
`ios/scripts/typing-sim.sh <UDID> --mode neural-swipe --input sample.jsonl --layout layout.json --out results.tsv`. Add `--native-only` to measure the cost of the second pass.

To reproduce bilingual ranking, use the same real English JSONL and keyboard layouts
for both encoder passes. Run from the repository root with NumPy, Numba, PyTorch
and the ExecuTorch 1.6.0 Python runtime. The native pass can also export the
Swedish-layout proxy for Swift. Keep these gesture files and outputs outside git.

```sh
python ios/scripts/swipe-blend-bench.py --model "$MODEL" --layout "$QWERTY" --swedish-layout "$SWEDISH" --input "$INPUT" --geometry native --out "$DIR/native.gz" --replay-input "$DIR/proxy.jsonl" --replay-layout "$DIR/layout.json"
python ios/scripts/swipe-blend-bench.py --model "$MODEL" --layout "$QWERTY" --swedish-layout "$SWEDISH" --input "$INPUT" --geometry canonical --english-beam-only --out "$DIR/canonical.gz"
python ios/scripts/merge-swipe-layouts.py --native "$DIR/native.gz" --canonical "$DIR/canonical.gz" --out "$DIR/both.gz"
ios/scripts/typing-sim.sh "$UDID" --mode swipe-geometry --layout "$DIR/layout.json" --input "$DIR/proxy.jsonl" --out "$DIR/geometry.jsonl"
python ios/scripts/score-deployed-swipe.py --neural "$DIR/both.gz" --geometry "$DIR/geometry.jsonl" --input "$INPUT" --out "$DIR/reference.tsv"
ios/scripts/typing-sim.sh "$UDID" --mode neural-swipe --layout "$DIR/layout.json" --input "$DIR/proxy.jsonl" --out "$DIR/swift.tsv"
```

Compare the first three TSV columns (gesture ID, label and first choice).
The reference runs the encoder, CTC beam and final ranking in Python. Both paths
use Swift's geometric candidates.

## Compare it with Apple's keyboard

`scripts/keyboard-parity.sh` puts Apple's keyboard and this one in the same state in a booted simulator, screenshots both, and compares them. Use a simulator made for it, because its host app takes the bundle ID `computer.see.ios`.

```sh
export AGENT_DEVICE=<the agent-device command bound to the simulator>
scripts/keyboard-parity.sh setup <simulator id>
scripts/keyboard-parity.sh run <simulator id> parity-out          # every state
scripts/keyboard-parity.sh run <simulator id> parity-out 'sv-hold-*'
```

`setup` builds the keyboard into a small host app with one text field, enables Apple's English and Swedish keyboards and this one, and grants Full Access. The first time, open the host, tap the field, hold the globe key and pick see.computer once. `run` drives 55 states: letters, shift and caps lock, the number and symbol pages, held keys with their previews, every return key enabled and greyed, the suggestion strip, URL and web search fields, and dark mode. Taps land where Apple's keys are, read from the host's accessibility tree, and the same points are replayed on this keyboard. For each state it writes `<state>-side.png` with Apple, this keyboard and the differing pixels in red, and `<state>-caps.tsv` with every key cap's frame, corner radius, colour and label offset on both. `summary.tsv` holds one row per state with the share of pixels that differ by more than 12 of 255 in any channel, the same share for the strip and the keys alone, and the largest key-frame gap.

iOS draws 17 pt of its own keyboard above a custom keyboard, cuts it off a few points above the bottom of Apple's last row, and delivers no touches below that line. This keyboard gives every row Apple's full cap height, keeps 5.5 pt of touch margin under the bottom caps, and sits about 9.5 pt higher than Apple's instead. So the summary also reports that uniform vertical offset (`offset_y`, the median over all caps) and, with it taken out, how far cap sizes and gaps differ (`shape_gap_pt`, and `row_shape_gaps` with the largest per row, top to bottom). The space bar is wider than Apple's because there is no emoji key, which shows in the bottom row's number.
