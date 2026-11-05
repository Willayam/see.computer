# see.computer for iPhone

A voice keyboard for iOS. It runs the Mac app's speech engine and text passes on the phone, so dictation stays on the device and reads the same dictionary. The App Store does not carry it, and two parts of it could not pass App Review. [What relies on undocumented iOS behavior](#what-relies-on-undocumented-ios-behavior) names them.

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

`ios/scripts/testflight.sh` archives a build and uploads it to App Store Connect. Create the app there first, with the bundle ID `computer.see.ios`, because only the website can do that. This build leaves out `HostCapture` and `HostReturn`, so after the app turns the mic on, you swipe back to your app yourself. Testers on your App Store Connect team get it without App Review.

## Using it

The whole keyboard is one button. Tap it, talk, then tap anywhere on it. Or hold it, talk, and let go. The X cancels. It has no space, delete or return key, so switch to another keyboard for those.

The words appear in the text field as you talk, underlined until the take is done, and the final text replaces them. If a field stops holding them, because the cursor moved, the rest show on the keyboard. A take you walk away from replaces its words if you are back within 10 seconds. After that the words come out of the field and the take stays in the app's history.

iOS gives keyboards no microphone, so the app has to hold it. When the app is not already listening, the first tap opens it, turns the mic on, and comes straight back. Dictation starts when you land. After that the mic stays on in the background, with the orange dot showing, and later taps never leave the app you are in. **Keep the mic ready for** sets how long. A phone call turns it off, and the next tap brings it back the same way.

While armed, the app holds the last 300 ms of audio and discards the rest. Nothing is kept until you tap.

Dictations are saved as one markdown file per day in the app's folder, visible in the Files app, in the same format as the Mac's history. Tap one in the app to copy it.

The keyboard hands over to the next keyboard in number, phone and email fields, and iOS replaces it in password fields.

## How it is built

The app owns the microphone, the engine and `state.json` in the shared App Group folder. The keyboard reads that file and sends Darwin notifications (`start`, `stop`, `cancel`, `ping`). It holds no audio and makes no decisions. It writes the words so far with `setMarkedText`, the call keyboards for Japanese use for text they have not committed. It decides whether the app is alive by whether a `ping` gets a `pong`, because a killed app cannot correct the file it left behind.

`core/` is a small Rust crate that compiles the Mac's own source files by path: `engine/`, `boost.rs`, `vocabulary.rs`, `filler.rs`, `numbers.rs`, `text.rs`, `align.rs` and `qos.rs`. `core/src/seams.rs` stands in for the few Mac modules they reach into. A change to any of those files reaches both apps. CI builds this crate so a Mac change cannot break it unnoticed.

The engine is Parakeet TDT 0.6b v3 (INT8, ONNX) on the CPU, as on the Mac. The Neural Engine would use less power, but [Sayboard](https://github.com/stanlsv/sayboard), an open source voice keyboard, treats it as blocked for background apps from iOS 27 and falls back to the CPU. The mapped weights stay out of the app's memory footprint, so it sits near 135 MB while loaded.

## What relies on undocumented iOS behavior

Each has a fallback, and each can break in any iOS update.

- `HostCapture` reads the host app's bundle id from a private UIKit callback, and `HostReturn` opens that app with a private system class. iOS 26.4 removed every supported way to do this. When either fails, the app shows a screen asking you to swipe right along the bottom edge.
- The keyboard opens the app by walking its responder chain to the `UIApplication` object. Apple documents opening the containing app from a keyboard as unsupported. When it fails, the keyboard asks you to open the app yourself.

## Verify it without a microphone

Debug builds replace the microphone when `Documents/test-input.wav` exists in the app's container. Every take then returns that file's audio, as `SEE_COMPUTER_AUDIO_FILE` does on the Mac. Use this in the simulator, because the input node goes through the Mac's audio service there and aborts the app if that service does not answer in time.

`scripts/bench-phone.sh <device id> <wav>` transcribes the file repeatedly on a connected iPhone and prints one JSON line per run with the time, the memory footprint, and whether the app was in the foreground. It needs a debug build and a model already on the phone.
