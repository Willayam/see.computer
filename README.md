# see.computer

Ultra-fast, local-first dictation and instant video links for the Mac. Free and open source (MIT). Website at [see.computer](https://see.computer).

## Install

Download the DMG from the [latest release](https://github.com/Willayam/see.computer/releases/latest), drag see.computer into Applications, and open it. It needs macOS 14 or later on Apple Silicon.

The app lives in the menu bar. On first launch it downloads the 670 MB speech model, and the pill at the bottom of the screen shows the percentage. macOS then asks for permissions, each the first time the feature that needs it runs. Microphone and Input Monitoring make hold-to-talk work. Accessibility lets the app paste for you. Screen Recording is only asked for the first time you hold the trigger with Shift. The tray menu has a shortcut to each pane, and the [Permissions](#permissions) section below says what each one is for.

Then click into any text field, hold the Globe key, say something, and let go.

If you would rather build it yourself, see [Build and run](#build-and-run).

Three gestures. That is the whole interface.

| Gesture | What happens |
|---|---|
| Hold **Globe** (Fn), speak, release | The spoken text is pasted at your cursor, in any app. Plain dictation stays local. |
| Double-tap **Globe** | Locks dictation open with nothing held. Tap Globe once to stop and paste. Esc cancels. |
| Hold **Globe**, double-tap **Shift**, release both | Locks screen recording and narration. Tap Globe once to stop and paste. Esc cancels. |
| Hold **Globe+Shift**, release to stop | Records the screen and microphone while held. On release, the recording is packaged into an agent-readable take folder. The paste contains the narration in quotes, a blank line, then a tail naming the captures and the path to `take.md`. Releasing before about 0.6 seconds discards the recording. |

The Globe key is the default. Hold it to dictate, or double-tap it to lock dictation open, shown with a blue ring. While holding it, tap Shift for a screenshot, hold Shift for a video clip, or double-tap Shift to lock video and narration. A single screenshot waits up to 300 ms for a possible second tap. After video locks, release both keys and use the computer normally. Tap the trigger once to stop, or press Esc to cancel. The pill shows a red ring while video is locked.

macOS can claim the same Globe taps. If Dictation is on with its "Press 🌐 twice" shortcut, or "Press 🌐 key to" is set to anything but "Do nothing", the tray shows a warning that opens the Keyboard settings. The app never changes system shortcuts itself. Choose Left Option or Right Option in the tray to use a different trigger; every trigger has the same gestures. Right Option is AltGr on a Swedish keyboard.

If the keyboard listener is interrupted, the app finishes the current take and rebuilds the listener with fresh gesture state. Controller cancellations, rejected starts, and missed key releases also reset the gesture decoder.

Dictation runs NVIDIA Parakeet TDT 0.6b v3 (INT8, ONNX) on the CPU. On an M3 Max a six-second English sentence comes back in about 200 ms, the median of eight runs of `see-computer transcribe` on `fixtures/en.wav`. The model is 670 MB and downloads on first launch into `~/Library/Application Support/see.computer/models/`.

Hesitation is dropped before anything is pasted. `um`, `uh` and `hm` go, along with the comma or full stop stuck to them, and the next word takes over the capital letter they were holding. Hedges like `like` and `actually` stay, because they are real words far more often than they are filler, and so does `er`, which is Swedish for "your". A dictation that was nothing but a hum pastes nothing at all.

Numbers are written the way you would read them, not the way they were said. Parakeet makes that choice on its own and gets it right about half the time, so `twenty percent` came back as words as often as `20%`. A grammar over the number words of English and Swedish runs after the filler pass. `two hundred and seventy nine` becomes `279`, `twenty-five thousand` becomes `25,000`, `Fable five point one` becomes `Fable 5.1`, `tjugo procent` becomes `20 %`, and `two million` stays `2 million`. The rule is the usual one. Ten and above are digits, and zero to nine stay words unless they carry a percent sign or a decimal point, so `one thing` and the Swedish article `en` are left alone. Digits already in the text pass through, and the stray space the model puts in `235 ,000` is closed.

Recordings land in `~/Documents/see.computer/`, next to the dictation history. Each `<stamp>/` take contains `take.md`, `transcript.json`, screenshots under `shots/`, and numbered clips under `clips/`. A clip keeps its movie at `clips/001/clip.mov` and its JPEG frames under `clips/001/frames/`. The paste puts the narration in quotes, leaves a blank line, then names the screenshots and clips before the `take.md` path. A human or a web chatbox gets the words even where a local path is dead. An agent that can read files can open the exact frames where each thing was said without decoding the video. If packaging fails or times out, the pill says so and the movie stays in `~/Documents/see.computer/`, where the tray panel still lists it and copies its `file://` link. The tray panel lists recordings among the recent dictations by their narration, under a video glyph where a dictation carries a waveform, and picking one copies that same text. `see-computer clip <mov>` moves an existing recording into a new take folder and prints its `take.md` path.

## Optional sharing beta

In version 0.1.1, turn on **Make recordings shareable** in Settings. It applies to new takes. Video chunks and locally compressed screenshots upload while you record. Transcription still runs on your Mac. Plain dictation does not create a share.

Anyone with the link can view the captures and transcript. The beta allows 10 shares per installation per month, keeps links for 30 days, and limits each share to 256 MiB and 1,500 files. Cancelled takes count toward the allowance. There is no paid plan yet.

Turning sharing off pauses uploads. Connection failures retry automatically, including after an app restart. A pending link includes the local take path in the pasted text. If publishing fails, recent items copy the local path. **Remove last shared link** queues deletion of the latest share. Local originals stay in Documents.

The service runs on private R2 storage in Cocco Ventures. See [the hosting instructions](services/shares/README.md) for limits and deployment, and [signed updates](docs/updates.md) for the release setup. Public distribution still needs Apple Developer ID signing and notarization.

## Dictation history

Transcripts are written to disk as plain text by default, with one markdown file per day in `~/Documents/see.computer/`. If macOS denies Documents access, they go to `~/Library/Application Support/see.computer/history/` instead.

Turn history off with **Save Dictation History** in the tray's **Settings**. Open **see.computer Folder** there to inspect or delete the plain files. There is no auto-pruning.

Files in Documents can also sync through iCloud Desktop & Documents, if you have enabled it.

## Custom vocabulary

Parakeet has never heard `Tauri` or `pnpm`, so it writes down the nearest thing it knows. Put the words it keeps missing in `~/Documents/see.computer/vocabulary.md`, one per line, and they are pushed into the decoder while it is still choosing — not substituted afterwards, which could not tell `Tauri` from the `tao ry` that a different sentence really did say.

```
# see.computer vocabulary
- Tauri
- Convex
- pnpm
```

Blank lines and `#` comments are skipped, a leading `- ` is allowed so the file reads as a list, and a tab-separated number after a term pushes that one harder. The file is re-read whenever it changes, so an edit takes effect on the next dictation with no restart. Case matters: the model capitalises, so write terms the way you want them written.

Terms are matched against the model's own tokens, so a word it has no way to spell — one in a script the model does not cover — is reported on stderr and skipped rather than silently ignored.

A term only gets its bonus when the model was already considering it: within `SEE_COMPUTER_BOOST_MARGIN` logits of its own best guess. That gate is what keeps a long list safe. A term the audio rules out is never in the running, so adding the sixtieth word does not make the app start hearing the first one everywhere. Without it, a sixty-term list turned `pnpm build` into `PNPM Buildship`.

`SEE_COMPUTER_VOCABULARY` points at a different file, `SEE_COMPUTER_BOOST_SCALE` changes how hard an eligible term is pushed, and `SEE_COMPUTER_BOOST_DEPTH` adds a bonus that grows the further into a term the decoder is — off by default, because it entrenches wrong turns as readily as right ones.

A bonus can win a close call, not overturn a confident one. Words the model is sure it heard differently will still come out wrong, however many times you list them.

## Build and run

Requires macOS 14 or later on Apple Silicon, Xcode command line tools, Rust 1.85 or later, and `cargo install tauri-cli --version "^2"`.

```sh
cd src-tauri
cargo tauri build --debug --bundles app
open target/debug/bundle/macos/see.computer.app
```

Set `APPLE_SIGNING_IDENTITY` to a signing identity before building so macOS keeps the permissions below across rebuilds. An unsigned build is re-prompted after every rebuild.

`cargo tauri dev` runs the app unbundled. Permission prompts then attach to the terminal rather than to see.computer, so prefer the bundle when testing paste and recording.

## Permissions

macOS asks for these the first time each feature runs. The tray menu has a shortcut to each pane under **System Settings > Privacy & Security**.

Another dictation app listening on the same key doubles every input. Quit the other app or give it a different trigger.

- **Accessibility**: needed to press Command+V in the app you are dictating into. Without it the text is still on your clipboard and the pill says so.
- **Input Monitoring**: needed for bare-modifier hold-to-talk and modifier+Shift recording.
- **Microphone**: dictation, and the audio track of recordings.
- **Screen Recording**: the video hotkey.

## Verify it without a microphone

Environment variables provide deterministic seams for local checks.

- `SEE_COMPUTER_AUDIO_FILE=<wav>` replaces the microphone. Every dictation returns that file's audio.
- `SEE_COMPUTER_STATE_LOG=<path>` overrides the default local trace at `~/Library/Application Support/see.computer/events.tsv`. It records trigger/Shift edges, listener recovery, and controller transitions as `<unix ms>\t<from>\t<message>\t<to>`. It contains no typed characters, audio, or transcripts. The log starts over at 1 MiB.
- `SEE_COMPUTER_HISTORY_DIR=<directory>` writes dictation history directly to that directory without fallback or healing.

`scripts/launch-for-verification.sh [wav] [trail]` launches the built bundle with both set. `scripts/press-mod.swift` posts the modifier gestures as real keyboard events so a script can drive the app, and `scripts/click-tray.swift` opens the menu panel the same way. `see-computer transcribe <wav>` (the binary inside the bundle) transcribes a file and prints the text without starting the GUI.

`scripts/bench-footprint.sh` is the memory and latency ruler. `scripts/bench-under-load.sh [runs] [hogs] [qos]` runs the same transcription while `scripts/busy.swift` keeps the machine busy, which is where the thread scheduling classes earn their keep. `scripts/bench-cold-recording.sh` evicts the memory-mapped model weights and measures the first recording afterwards, which is where the press-time engine warm earns its keep.

```sh
cd src-tauri && cargo test
./target/debug/see-computer transcribe ../fixtures/en.wav
```

## How it is built

Tauri v2 with a Rust backend. The only web content is `ui/pill.html`, the small overlay at the bottom of the screen; there is no bundler and no Node toolchain.

The menu behind the tray icon is a non-activating `NSPanel` filled with `NSGlassEffectView`, built in `native/menu.m` from a row list `tray.rs` rebuilds on every open. Its main view centers recent dictations and recordings. Settings expands inline with folder, startup, and history controls, while trigger and permission choices open beside it. Model recovery appears only when the model is unavailable. The panel never becomes key, so the app being dictated into keeps its focus. An `NSMenu` would have run a modal tracking loop on the main thread and could not have carried the material.

One controller thread owns the app's state, an enum with one variant per thing the app can be doing (`Idle`, `Dictating`, `Transcribing`, `Packaging`, `Pasting`). Everything else sends it a message: the global-shortcut handler, the engine worker that owns the Parakeet model, the native recorder teardown thread, and the paste thread that owns the clipboard. Nothing slow runs on the hotkey thread, and dictating while recording cannot be expressed. Every thread names its scheduling class in `qos.rs` rather than taking the default, which the macOS scheduler ranks below anything the user is looking at: the event tap, the controller and the paste run user-interactive, the engine runs user-initiated, and upkeep runs utility. The pill window is created non-focusable and ordered in once with `orderFrontRegardless`, so it can never take the keystroke that pastes.

ScreenCaptureKit captures video into a hardware H.264 encoder at up to 2560 × 1440, 15 fps and 2 Mbps. Microphone audio uses mono AAC at 64 kbps. Completed one-second fragments are written atomically to disk. The uploader reads those files on a utility thread. Screenshots use `/usr/sbin/screencapture` and are compressed to JPEG locally for upload.

## License

MIT. See `LICENSE`. The speech model and the vendored crate carry their own licenses, listed in `THIRD_PARTY.md`.
