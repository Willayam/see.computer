# see.computer

Ultra-fast, local-first dictation and instant video links for the Mac. Free and open source (MIT).

Two gestures. That is the whole interface.

| Gesture | What happens |
|---|---|
| Hold **Left Option**, speak, release | The spoken text is pasted at your cursor, in any app. Audio never leaves the machine. |
| Hold **Left Option+Shift**, release to stop | Records the screen and microphone while held. On release, the recording is packaged into an agent-readable clip folder and the path to its `clip.md` is put on your clipboard and pasted at your cursor. Releasing before about 0.6 seconds discards the recording. |

Choose Left Option, Right Option, Fn (Globe), or the legacy Option+Space trigger from the tray. Right Option is AltGr on the Swedish keyboard layout. The recording gesture always follows the trigger: whatever key you pick, hold it with Shift to record and release to stop. The legacy Option+Space and Command+Shift+Option+Space chords are active only when the Option+Space trigger is selected.

Dictation runs NVIDIA Parakeet TDT 0.6b v3 (INT8, ONNX) on the CPU. On an M3 Max a six-second Swedish sentence comes back in about 155 ms. The model is 670 MB and downloads on first launch into `~/Library/Application Support/see.computer/models/`.

Recordings land in `~/Movies/see.computer/`. Next to each `<stamp>.mov` the app writes `<stamp>/` containing `clip.md` (a timestamped transcript with a screen frame inline at each sentence), `transcript.json` (the same segments, machine-readable), and `frames/` (JPEG stills, at most 16, extracted with AVFoundation). Paste the `clip.md` path into any agent that can read files and it can read what was said and open the exact frames where it was said, without decoding the video. If transcription or packaging fails or times out, the plain `file://` link to the `.mov` is pasted instead. `see-computer clip <mov>` (the binary inside the bundle) rebuilds the folder for any existing recording and prints the `clip.md` path.

## Dictation history

Transcripts are written to disk as plain text by default, with one markdown file per day in `~/Documents/see.computer/`. If macOS denies Documents access, they go to `~/Library/Application Support/see.computer/history/` instead.

Turn history off with **Save Dictation History** in the tray's **Settings**. There is no auto-pruning; these are plain files you can open or delete.

The audio-never-leaves-the-machine promise does not extend to transcript files in Documents when iCloud Desktop & Documents syncing is on.

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
- **Input Monitoring**: needed for bare-modifier hold-to-talk and modifier+Shift recording. The legacy Option+Space chord fallback needs only Accessibility.
- **Microphone**: dictation, and the audio track of recordings.
- **Screen Recording**: the video hotkey.

## Verify it without a microphone

Environment variables provide deterministic seams for local checks.

- `SEE_COMPUTER_AUDIO_FILE=<wav>` replaces the microphone. Every dictation returns that file's audio.
- `SEE_COMPUTER_STATE_LOG=<path>` appends one line per state transition: `<unix ms>\t<from>\t<message>\t<to>`.
- `SEE_COMPUTER_HISTORY_DIR=<directory>` writes dictation history directly to that directory without fallback or healing.

`scripts/launch-for-verification.sh [wav] [trail]` launches the built bundle with both set. `scripts/press-hotkey.swift` posts the chords as real keyboard events so a script can drive the app, and `scripts/click-tray.swift` opens the menu panel the same way. `see-computer transcribe <wav>` (the binary inside the bundle) transcribes a file and prints the text without starting the GUI.

`scripts/bench-footprint.sh` is the memory and latency ruler. `scripts/bench-under-load.sh [runs] [hogs] [qos]` runs the same transcription while `scripts/busy.swift` keeps the machine busy, which is where the thread scheduling classes earn their keep.

```sh
cd src-tauri && cargo test
./target/debug/see-computer transcribe ../fixtures/en.wav
```

## How it is built

Tauri v2 with a Rust backend. The only web content is `ui/pill.html`, the small overlay at the bottom of the screen; there is no bundler and no Node toolchain.

The menu behind the tray icon is a non-activating `NSPanel` filled with `NSGlassEffectView`, built in `native/menu.m` from a row list `tray.rs` rebuilds on every open. Its main view centers recent dictations and recordings; trigger, folder, startup, model, and permission controls live in an in-place Settings view. It never becomes key, so the app being dictated into keeps its focus. An `NSMenu` would have run a modal tracking loop on the main thread and could not have carried the material.

One controller thread owns the app's state, an enum with one variant per thing the app can be doing (`Idle`, `Dictating`, `Transcribing`, `Recording`, `Finalizing`, `Pasting`). Everything else sends it a message: the global-shortcut handler, the engine worker that owns the Parakeet model, the thread that waits for `screencapture` to finish, and the paste thread that owns the clipboard. Nothing slow runs on the hotkey thread, and dictating while recording cannot be expressed. Every thread names its scheduling class in `qos.rs` rather than taking the default, which the macOS scheduler ranks below anything the user is looking at: the event tap, the controller and the paste run user-interactive, the engine runs user-initiated, and upkeep runs utility. The pill window is created non-focusable and ordered in once with `orderFrontRegardless`, so it can never take the keystroke that pastes.

Screen recording spawns `/usr/sbin/screencapture -v -g -x` and stops it with SIGINT. ScreenCaptureKit and the cloud link tier are later variants behind the same `Recorder` and `Share` types.
