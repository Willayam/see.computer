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

## Keyboard word data

`ios/Keyboard/words-en.bin` and `words-sv.bin` hold word and word-pair frequencies built by `ios/scripts/build-typing-lexicon.py` from these sources. The full AOSP license and the attribution lines are in `ios/THIRD_PARTY_NOTICES.md`.

- **AOSP LatinIME** English (US) and Swedish wordlists, Apache-2.0.
  https://android.googlesource.com/platform/packages/inputmethods/LatinIME/+/refs/heads/main/dictionaries/
- **Leipzig Corpora Collection** `eng_news_2023_1M`, `eng-com_web-public_2018_1M`, `swe_news_2023_1M` and `swe_news_2022_1M`, CC BY 4.0.
  https://wortschatz.uni-leipzig.de/en/download
- **Mozilla Common Voice** Sentence Collector, English and Swedish, CC0.

The typing simulator's eval sentences in `ios/tests/typing-eval/` come from Tatoeba under CC BY 2.0 FR. They are not shipped in the app.

## Adapted code

`ios/Keyboard/HostCapture.m` adapts the arbiter hook from **KeyboardHostBundleID** by editorss, MIT.
https://github.com/Muskupecli/KeyboardHostBundleID

## Runtime and framework

The hosted sharing viewer includes the hls.js 1.7.2 light build, Apache-2.0. Its license is kept at `services/shares/public/hls-LICENSE`. Source at https://github.com/video-dev/hls.js.

- **ONNX Runtime** (Microsoft, MIT), through the `ort` crate (MIT OR Apache-2.0).
- **Tauri** (MIT OR Apache-2.0).
- All other Rust dependencies are listed in `src-tauri/Cargo.lock` with their licenses recorded on crates.io.
