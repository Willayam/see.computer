# see.computer

Ultra-fast, local-first dictation and instant video links for the Mac. Free and open source (MIT).

Two gestures. That is the whole interface.

| Gesture | What happens |
|---|---|
| Hold **Left Option**, speak, release | The spoken text is pasted at your cursor, in any app. Audio never leaves the machine. |
| **Left Option+Shift** | Press once to start a screen and microphone recording, press again to stop (it is a toggle, not hold-to-record). On stop, a link to the `.mov` is on your clipboard and pasted at your cursor. |

Choose Left Option, Right Option, Fn (Globe), or the legacy Option+Space trigger from the tray. Right Option is AltGr on the Swedish keyboard layout. The recording gesture always follows the trigger: whatever key you pick, add Shift to record. The legacy Option+Space and Command+Shift+Option+Space chords are active only when the Option+Space trigger is selected.

Dictation runs NVIDIA Parakeet TDT 0.6b v3 (INT8, ONNX) on the CPU. On an M3 Max a six-second Swedish sentence comes back in about 155 ms. The model is 670 MB and downloads on first launch into `~/Library/Application Support/see.computer/models/`.

The link is a `file://` URL in v1. Recordings land in `~/Movies/see.computer/`.

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

Two environment variables exist for deterministic checks. Both are read once in `main.rs` and nowhere else.

- `SEE_COMPUTER_AUDIO_FILE=<wav>` replaces the microphone. Every dictation returns that file's audio.
- `SEE_COMPUTER_STATE_LOG=<path>` appends one line per state transition: `<unix ms>\t<from>\t<message>\t<to>`.

`scripts/launch-for-verification.sh [wav] [trail]` launches the built bundle with both set. `scripts/press-hotkey.swift` posts the chords as real keyboard events so a script can drive the app, and `scripts/click-tray.swift` opens the menu panel the same way. `see-computer transcribe <wav>` (the binary inside the bundle) transcribes a file and prints the text without starting the GUI.

`scripts/bench-footprint.sh` is the memory and latency ruler. `scripts/bench-under-load.sh [runs] [hogs] [qos]` runs the same transcription while `scripts/busy.swift` keeps the machine busy, which is where the thread scheduling classes earn their keep.

```sh
cd src-tauri && cargo test
./target/debug/see-computer transcribe ../fixtures/en.wav
```

## How it is built

Tauri v2 with a Rust backend. The only web content is `ui/pill.html`, the small overlay at the bottom of the screen; there is no bundler and no Node toolchain.

The menu behind the tray icon is a non-activating `NSPanel` filled with `NSGlassEffectView`, built in `native/menu.m` from a row list `tray.rs` rebuilds on every open. It never becomes key, so the app being dictated into keeps its focus. An `NSMenu` would have run a modal tracking loop on the main thread and could not have carried the material.

One controller thread owns the app's state, an enum with one variant per thing the app can be doing (`Idle`, `Dictating`, `Transcribing`, `Recording`, `Finalizing`, `Pasting`). Everything else sends it a message: the global-shortcut handler, the engine worker that owns the Parakeet model, the thread that waits for `screencapture` to finish, and the paste thread that owns the clipboard. Nothing slow runs on the hotkey thread, and dictating while recording cannot be expressed. Every thread names its scheduling class in `qos.rs` rather than taking the default, which the macOS scheduler ranks below anything the user is looking at: the event tap, the controller and the paste run user-interactive, the engine runs user-initiated, and upkeep runs utility. The pill window is created non-focusable and ordered in once with `orderFrontRegardless`, so it can never take the keystroke that pastes.

Screen recording spawns `/usr/sbin/screencapture -v -g -x` and stops it with SIGINT. ScreenCaptureKit and the cloud link tier are later variants behind the same `Recorder` and `Share` types.
