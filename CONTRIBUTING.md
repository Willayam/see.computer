# Contributing

Bug reports and pull requests are welcome. Keep both small and specific.

## Before you open a pull request

```sh
cd src-tauri
cargo fmt
cargo clippy --all-targets
cargo test
```

CI runs the same three commands on an Apple Silicon runner.

Build the bundle and try the change in the real app before sending it. The README's "Verify it without a microphone" section explains how to drive the app with a WAV file and real modifier key events, so a change to the gesture or paste path can be checked from a script.

## Ground rules

- Audio never leaves the machine. Any change that adds a network call needs a very good reason, and the README has to say so.
- One controller thread owns the state. New work goes on its own thread and sends the controller a message. Nothing slow on the hotkey path.
- The vendored `parakeet-rs` stays upstream 0.3.7 plus the delta in its `FORK.md`. Do not grow the fork.
- Prose in the README and in commit messages follows the tone already there. Say what the change does and why, with the number or the mechanism.

## Reporting a bug

Include your macOS version, the chip, the trigger key you use, and whether another dictation app was running. If a transcription is wrong, attach the WAV if you can and what you expected. `see-computer transcribe <wav>` inside the bundle reproduces the engine without the GUI.
