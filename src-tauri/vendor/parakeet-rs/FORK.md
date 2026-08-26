# Why this is vendored

`parakeet-rs` runs TDT greedy decoding inside the crate: `ParakeetTDTModel::forward`
argmaxes the joint logits frame by frame and never surfaces them. Custom-vocabulary
boosting has to add a bonus to those logits *before* the argmax — after the fact all
you can do is string replacement, which cannot recover a word the decoder never
emitted.

So this is upstream 0.3.7 verbatim plus one addition:

- `src/bias.rs` — the `TokenBias` trait (new file).
- `src/model_tdt.rs` — `forward` and `greedy_decode` take `Option<&mut dyn TokenBias>`
  and call it on the vocab slice of the logits before the argmax, then report each
  emitted token.
- `src/parakeet_tdt.rs` — `transcribe_samples_with_bias`, the public entry point.
- `src/lib.rs` — re-exports `TokenBias`.

Everything else is untouched, examples and dev-dependencies aside. Keep it that way:
the delta is meant to go upstream as a pull request, after which this directory and
the `[patch.crates-io]` entry in `src-tauri/Cargo.toml` both go away.

Upstream: https://github.com/altunenes/parakeet-rs (MIT OR Apache-2.0)
