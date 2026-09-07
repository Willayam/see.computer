# Third-party notices

see.computer is MIT licensed (see `LICENSE`). It ships with, or downloads at first launch, the following work by other people.

## Speech model

**Parakeet TDT 0.6b v3** by NVIDIA. Licensed under [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/).
https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3

The app downloads the INT8 ONNX conversion published by **istupakov**, also under CC BY 4.0.
https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx

The model is not in this repository. It is fetched on first launch into `~/Library/Application Support/see.computer/models/`.

## Vendored crate

**parakeet-rs** 0.3.7 by altunenes, MIT OR Apache-2.0, vendored at `src-tauri/vendor/parakeet-rs` with one addition described in `src-tauri/vendor/parakeet-rs/FORK.md`. The original license file is kept alongside it.
https://github.com/altunenes/parakeet-rs

## Runtime and framework

- **ONNX Runtime** (Microsoft, MIT), through the `ort` crate (MIT OR Apache-2.0).
- **Tauri** (MIT OR Apache-2.0).
- All other Rust dependencies are listed in `src-tauri/Cargo.lock` with their licenses recorded on crates.io.
