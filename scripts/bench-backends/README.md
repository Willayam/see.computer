# Local transcription benchmark

Compare the app's INT8 ONNX Parakeet v3 configuration with MLX GPU inference and optional ONNX Core ML execution. This builds a separate executable and does not change the app.

## Run

Run these commands from the repository root on an Apple Silicon Mac. The app's prepared ONNX model must already exist in its normal Application Support directory.

```sh
uv venv /tmp/see-backend-bench-venv --python 3.12
uv pip install --python /tmp/see-backend-bench-venv/bin/python -r scripts/bench-backends/requirements.txt
cargo build --release --locked --manifest-path scripts/bench-backends/Cargo.toml
```

Download the MLX weights before timing. The checkpoint is about 2.5 GB. Pin the revision used for the recorded results.

```sh
/tmp/see-backend-bench-venv/bin/python - <<'PY'
from huggingface_hub import snapshot_download
print(snapshot_download(
    "mlx-community/parakeet-tdt-0.6b-v3",
    revision="ed2b7e8c15f9aaa0b5772e2efb986255eaef7e15",
    allow_patterns=["*.json", "*.safetensors"],
))
PY
```

Pass the printed snapshot directory as `--mlx-model` below.

```sh
python3 scripts/bench-backends/run.py \
  --python /tmp/see-backend-bench-venv/bin/python \
  --mlx-model /path/to/downloaded/snapshot \
  --out .audit/backend-bench/results
python3 scripts/bench-backends/summarize.py .audit/backend-bench/results/raw.jsonl
```

The runner briefly adds CPU load during each `cpu-load` case. It terminates its own load process after every case, including on interruption. It does not close or reprioritize other apps. Each backend runs alone, with 10 repetitions per clip in each of two rounds. The second round reverses both backend and condition order. There are 20 measurements per backend, condition, and clip.

## What is timed

WAV loading and model initialization happen before timing. Each model receives one warmup per input shape. Measurements include audio feature extraction, recognition, decoding, and sentence output. MLX synchronization brackets each measurement so queued GPU work is included. CPU execution uses the app's eight-thread configuration, memory-mapped weights, disabled prepacking, and user-initiated scheduling class. A four-thread CPU variant checks thread contention. Both implementations omit personal vocabulary and the app's final filler and number passes.

Normal load means the existing desktop workload. Added load means 14 Swift busy workers at user-initiated priority. These measurements do not cover memory pressure, GPU contention, microphone capture, keyboard handling, clipboard insertion, or real-time streaming.

`mixed.wav` concatenates the Swedish fixture and the English fixture without adding silence. It tests one language transition, not natural switching within a sentence. Both original clips remain unchanged.

`summary.csv` contains the median and nearest-rank empirical p95. `transcripts.json` contains every distinct output. `raw.jsonl` retains all samples, loading times, warmups, and errors. A p95 from 20 repeated samples describes this small workload only.

## Core ML compatibility probes

Core ML requests allow CPU fallback for unsupported operations. Successful registration alone does not prove GPU or Neural Engine execution. The executable uses `error_on_failure` so registration failures are not silently accepted. Set `SEE_BENCH_PROFILE=1` for partition and compute-plan diagnostics, but do not use profiled timings in the comparison.

```sh
python3 scripts/bench-backends/run.py \
  --python /tmp/see-backend-bench-venv/bin/python \
  --modes coreml-gpu coreml-ane --runs 1 --rounds 1 \
  --out .audit/backend-bench/coreml-probes
```

The default conversion format is MLProgram. Set `SEE_BENCH_COREML_LEGACY=1` to probe the older NeuralNetwork format. These use the app's ONNX model. A native Core ML export is a different approach and needs its own benchmark.

## Checks

```sh
cargo fmt --check --manifest-path scripts/bench-backends/Cargo.toml
cargo clippy --release --locked --manifest-path scripts/bench-backends/Cargo.toml -- -D warnings
python3 -m py_compile scripts/bench-backends/run.py scripts/bench-backends/mlx_bench.py scripts/bench-backends/summarize.py
```
