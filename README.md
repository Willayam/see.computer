# see.computer

Ultra-fast, local-first dictation and instant video links for the Mac. Free and open source (MIT).

Three gestures. That is the whole interface.

| Gesture | What happens |
|---|---|
| Hold **Left Option**, speak, release | The spoken text is pasted at your cursor, in any app. Audio never leaves the machine. |
| Double tap **Left Option**, speak, tap to finish | The same take, held open with nothing on the trigger. Made for long narration, where holding a key for two minutes is the thing that gets in the way. One tap of the trigger finishes it and pastes, Esc throws it away. To take a shot or a clip inside a locked take, hold the trigger again and use Shift as usual; Shift on its own is left alone, so you can still type a capital letter. |
| Hold **Left Option+Shift**, release to stop | Records the screen and microphone while held. On release, the recording is packaged into an agent-readable take folder. The paste contains the narration in quotes, a blank line, then a tail naming the captures and the path to `take.md`. Releasing before about 0.6 seconds discards the recording. |

Choose Left Option, Right Option, or Fn (Globe) from the tray. Right Option is AltGr on the Swedish keyboard layout. The recording gesture always follows the trigger: whatever key you pick, hold it with Shift to record and release to stop, and double tap it to lock a take open. Both halves of the double tap have to be taps, so an ordinary hold is never mistaken for one and hold-to-talk starts exactly as fast as it always did. A locked take wears a lit ring around the pill, because a live microphone nobody is touching should never look like an idle one. The lock lives in the event tap that decodes the gesture: if the tap dies, the lock goes with it and the take finishes normally rather than leaving a microphone open that nothing can close.

Dictation runs NVIDIA Parakeet TDT 0.6b v3 (INT8, ONNX) on the CPU. On an M3 Max a six-second Swedish sentence comes back in about 155 ms. The model is 670 MB and downloads on first launch into `~/Library/Application Support/see.computer/models/`.

Hesitation is dropped before anything is pasted. `um`, `uh` and `hm` go, along with the comma or full stop stuck to them, and the next word takes over the capital letter they were holding. Hedges like `like` and `actually` stay, because they are real words far more often than they are filler, and so does `er`, which is Swedish for "your". A dictation that was nothing but a hum pastes nothing at all.

Recordings land in `~/Documents/see.computer/`, next to the dictation history. Each `<stamp>/` take contains `take.md`, `transcript.json`, screenshots under `shots/`, and numbered clips under `clips/`. A clip keeps its movie at `clips/001/clip.mov` and its JPEG frames under `clips/001/frames/`. The paste puts the narration in quotes, leaves a blank line, then names the screenshots and clips before the `take.md` path. A human or a web chatbox gets the words even where a local path is dead. An agent that can read files can open the exact frames where each thing was said without decoding the video. If packaging fails or times out, the pill says so and the movie stays in `~/Documents/see.computer/`, where the tray panel still lists it and copies its `file://` link. The tray panel lists recordings among the recent dictations by their narration, under a video glyph where a dictation carries a waveform, and picking one copies that same text. `see-computer clip <mov>` moves an existing recording into a new take folder and prints its `take.md` path.

## Dictation history

Transcripts are written to disk as plain text by default, with one markdown file per day in `~/Documents/see.computer/`. If macOS denies Documents access, they go to `~/Library/Application Support/see.computer/history/` instead.

Turn history off with **Save Dictation History** in the tray's **Settings**. There is no auto-pruning; these are plain files you can open or delete.

The audio-never-leaves-the-machine promise does not extend to transcript files in Documents when iCloud Desktop & Documents syncing is on.

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

Another dictation app listening on the same key doubles every input. see.computer warns when a known one (Hex, Wispr Flow, Superwhisper, …) is running; quit it or change the trigger.

- **Accessibility**: needed to press Command+V in the app you are dictating into. Without it the text is still on your clipboard and the pill says so.
- **Input Monitoring**: needed for bare-modifier hold-to-talk and modifier+Shift recording.
- **Microphone**: dictation, and the audio track of recordings.
- **Screen Recording**: the video hotkey.

## Verify it without a microphone

Environment variables provide deterministic seams for local checks.

- `SEE_COMPUTER_AUDIO_FILE=<wav>` replaces the microphone. Every dictation returns that file's audio.
- `SEE_COMPUTER_STATE_LOG=<path>` appends one line per state transition: `<unix ms>\t<from>\t<message>\t<to>`.
- `SEE_COMPUTER_HISTORY_DIR=<directory>` writes dictation history directly to that directory without fallback or healing.

`scripts/launch-for-verification.sh [wav] [trail]` launches the built bundle with both set. `scripts/press-mod.swift` posts the modifier gestures as real keyboard events so a script can drive the app, and `scripts/click-tray.swift` opens the menu panel the same way. `see-computer transcribe <wav>` (the binary inside the bundle) transcribes a file and prints the text without starting the GUI.

`scripts/bench-footprint.sh` is the memory and latency ruler. `scripts/bench-under-load.sh [runs] [hogs] [qos]` runs the same transcription while `scripts/busy.swift` keeps the machine busy, which is where the thread scheduling classes earn their keep. `scripts/bench-cold-recording.sh` evicts the memory-mapped model weights and measures the first recording afterwards, which is where the press-time engine warm earns its keep.

```sh
cd src-tauri && cargo test
./target/debug/see-computer transcribe ../fixtures/en.wav
```

## How it is built

Tauri v2 with a Rust backend. The only web content is `ui/pill.html`, the small overlay at the bottom of the screen; there is no bundler and no Node toolchain.

The menu behind the tray icon is a non-activating `NSPanel` filled with `NSGlassEffectView`, built in `native/menu.m` from a row list `tray.rs` rebuilds on every open. Its main view centers recent dictations and recordings; trigger, folder, startup, model, and permission controls live in an in-place Settings view. It never becomes key, so the app being dictated into keeps its focus. An `NSMenu` would have run a modal tracking loop on the main thread and could not have carried the material.

One controller thread owns the app's state, an enum with one variant per thing the app can be doing (`Idle`, `Dictating`, `Transcribing`, `Packaging`, `Pasting`). Everything else sends it a message: the global-shortcut handler, the engine worker that owns the Parakeet model, the thread that waits for `screencapture` to finish, and the paste thread that owns the clipboard. Nothing slow runs on the hotkey thread, and dictating while recording cannot be expressed. Every thread names its scheduling class in `qos.rs` rather than taking the default, which the macOS scheduler ranks below anything the user is looking at: the event tap, the controller and the paste run user-interactive, the engine runs user-initiated, and upkeep runs utility. The pill window is created non-focusable and ordered in once with `orderFrontRegardless`, so it can never take the keystroke that pastes.

Screen recording spawns `/usr/sbin/screencapture -v -g -x` and stops it with SIGINT. ScreenCaptureKit is a later variant behind the same `Recorder` type.
