# Third-party notices

see.computer is MIT licensed (see `LICENSE`). It ships with, or downloads at first launch, the following work by other people.

## Speech model

**Parakeet TDT 0.6b v3** by NVIDIA. Licensed under [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/).
https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3

The app downloads the INT8 ONNX conversion published by **istupakov**, also under CC BY 4.0.
https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx

The model is not in this repository. The Mac app fetches it on first launch into `~/Library/Application Support/see.computer/models/`, and the iPhone app into its own Application Support folder.

## Vendored crate

**parakeet-rs** 0.3.7 by altunenes, MIT OR Apache-2.0, vendored at `src-tauri/vendor/parakeet-rs` with one addition described in `src-tauri/vendor/parakeet-rs/FORK.md`. The original license file is kept alongside it.
https://github.com/altunenes/parakeet-rs

## Adapted code

`ios/Keyboard/HostCapture.m` adapts the arbiter hook from **KeyboardHostBundleID** by editorss, MIT.
https://github.com/Muskupecli/KeyboardHostBundleID

## Runtime and framework

The hosted sharing viewer includes the hls.js 1.7.2 light build, Apache-2.0. Its license is kept at `services/shares/public/hls-LICENSE`. Source at https://github.com/video-dev/hls.js.

- **ONNX Runtime** (Microsoft, MIT), through the `ort` crate (MIT OR Apache-2.0).
- **Tauri** (MIT OR Apache-2.0).
- All other Rust dependencies are listed in `src-tauri/Cargo.lock` with their licenses recorded on crates.io.
