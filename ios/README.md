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

`ios/scripts/testflight.sh` archives a build and uploads it to App Store Connect. Create the app there first, with the bundle ID `computer.see.ios`, because only the website can do that. In this build, open see.computer and turn the mic on before dictating. Switch back to the app you were typing in and tap **Dictate**. While the mic stays ready, you can start and finish later takes from the keyboard.

## Using it

Tap **Dictate** above the letters, talk, then tap **Done** in the same spot. Or hold Dictate, talk, and let go. You can type while the mic keeps recording. The live speech preview then moves into the top row. Done inserts the transcript in the original field after your keyboard edits. If the field, selection, or surrounding text has changed outside the keyboard, the full take stays in History. The X cancels. When the transcript lands, select a word in the host app and type its replacement on the same keyboard. Tap Dictate again to continue speaking.

**Typing languages** in the see.computer app follows the iPhone’s preferred languages by default. Choose **English & Svenska** once to keep Å, Ä and Ö on the letter rows in every app. Shift changes case, a double tap locks capitals, and **123** opens numbers and symbols. Hold and slide the space bar to position the cursor. Double-tap a word in the host app to use its Copy and Paste menu. Double-tap space after a letter or digit to insert a period and a space. Typing a space after a number returns to the letters.

The keyboard keeps where each finger landed. While you type, a tap near the edge of a key goes to the letter that more likely comes next, and a tap in the middle of a key always types that key. At a space or punctuation it reads the whole word again from the touch points, the words around it and English and Swedish word frequencies. That fixes neighbor keys, skipped, doubled and swapped letters, a space that landed on a letter, and a letter that landed on the space bar. It can also fix the word before. The bold suggestion is what space or punctuation will accept. Tap the quoted spelling to keep it, or press backspace immediately after a correction to restore what you typed. Tap a suggestion to finish a word or insert the next one. In bilingual mode the recent words help rank suggestions, and a word from either language can stay. The keyboard also uses the iPhone’s text replacements and the names iOS shares with it.

The keyboard scores nearby keys using English and Swedish word frequencies from the AOSP dictionaries, Leipzig news and web corpora, and Mozilla Common Voice. The bundled models run on device. Rebuild them with `python3 ios/scripts/build-typing-lexicon.py /path/to/cache`. [Data sources and licenses](THIRD_PARTY_NOTICES.md)

The words appear in the text field as you talk, underlined until the take is done, and the final text replaces them. If a field stops holding them, because the cursor moved, the rest show on the keyboard.

Leaving the keyboard does not end a take. The Dynamic Island and the Lock Screen show the mic while it is on, with a clock and the mic level during a take. **Done** there finishes the take, and **Turn off mic** turns the mic off between takes. A take ends by itself after 5 minutes away from the keyboard. Its text replaces the words in its own field if you come back within 10 minutes. Sending a message while the words are still underlined also ends the take, and the final text is not typed again. If the app dies mid-take, the words stay in the field as plain text. A finished take stays in History when the keyboard cannot verify its insertion point.

iOS gives keyboards no microphone, so the app has to hold it. In a direct Xcode install, when the app is not already listening, the first tap opens it, turns the mic on, and returns to the host app if iOS allows it. Dictation starts when you land. After that the mic stays on in the background, with the orange dot showing, and later taps never leave the app you are in. **Keep the mic ready for** sets how long. A phone call turns it off, and the next tap brings it back the same way.

While armed, the app holds the last 300 ms of audio and discards the rest. Nothing is kept until you tap.

Dictations are saved as one markdown file per day in the app's folder, visible in the Files app, in the same format as the Mac's history. Tap one in the app to copy it.

The keyboard hands over to the next keyboard in number, phone and email fields, and iOS replaces it in password fields.

## How it is built

The app owns the microphone, the engine and `state.json` in the shared App Group folder. The keyboard reads that file and sends Darwin notifications (`start`, `stop`, `cancel`, `ping`, `away`, `sent`). It holds no audio. It checks the field identifier and fingerprints of the surrounding text before changing the host field. It writes the words so far with `setMarkedText`, the call keyboards for Japanese use for text they have not committed. It decides whether the app is alive by whether a `ping` gets a `pong`, because a killed app cannot correct the file it left behind. The `Island` widget extension draws the Live Activity, and only the app starts and updates it.

`core/` is a small Rust crate that compiles the Mac's own source files by path: `engine/`, `boost.rs`, `vocabulary.rs`, `filler.rs`, `numbers.rs`, `text.rs`, `align.rs` and `qos.rs`. `core/src/seams.rs` stands in for the few Mac modules they reach into. A change to any of those files reaches both apps. CI builds this crate so a Mac change cannot break it unnoticed.

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

Debug builds replace the microphone when `Documents/test-input.wav` exists in the app's container. Every take then returns that file's audio, as `SEE_COMPUTER_AUDIO_FILE` does on the Mac. Use this in the simulator, because the input node goes through the Mac's audio service there and aborts the app if that service does not answer in time.

`scripts/bench-phone.sh <device id> <wav>` transcribes the file repeatedly on a connected iPhone and prints one JSON line per run with the time, the memory footprint, and whether the app was in the foreground. It needs a debug build and a model already on the phone.

## Measure typing accuracy

`scripts/typing-sim.sh <simulator id> --out run.tsv` lays out the real keys in a booted simulator and types 1,600 held-out English and Swedish sentences three times: on key centers, with a touch spread of 0.2 key widths, and with 0.3. It adds a skipped, doubled or swapped letter at 0.5% each per letter. It prints the word error rate per level and `noisy_wer`, the rate over the two noisy levels. `python3 scripts/typing-sim-compare.py before.tsv after.tsv` compares two runs sentence by sentence with a 95% interval. Tune on `--set dev` and confirm on `--set test`. One run takes about three minutes.

Swipe across letter keys and lift to insert a word. The keyboard supplies the space before the next swiped word. Tap an alternative in the suggestion row to replace the word, or press delete immediately to remove it.

Use `--mode swipe` with the same simulator command and corpus. The generator collapses adjacent repeated letters, omits internal apostrophes from the path and taps standalone punctuation, digits and single letters. It perturbs each target independently by Gaussian noise of 0, 0.20 or 0.35 key pitches per axis. Noisy strokes also vary the endpoints along their approach direction. The finger slows at each target, moves at a speed chosen per word and samples at 60 to 120 Hz. A moving average cuts corners. The clean level keeps the targets centered. The generator uses the sentence, level, layout and seed. It has no access to decoder scores.

WER counts substitutions, insertions and omissions in the resulting field, including missed gesture starts. Decode timing covers successful gesture recognition through candidate ranking, excluding drawing and host text edits. The ten-letter timing groups by reference word length. Run `--mode swipe --endpoints` to score a control that reads only the first and last keys. This synthetic benchmark does not measure actual finger trajectories, correlated drift or host-app touch delivery.
