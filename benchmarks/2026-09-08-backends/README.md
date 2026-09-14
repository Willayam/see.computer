# Local transcription on the M3 Max

MLX GPU inference substantially reduced the slowdown caused by CPU contention. Keep the app's default unchanged until its memory cost and mixed-language accuracy have been addressed. Reducing CPU inference to four threads did not help.

Measured on 2026-09-08 on this Mac, an Apple M3 Max with 14 CPU cores and 36 GiB of unified memory, running macOS 27.0 build 26A5425a. The app source was at `48bbc916ff793103d860093eacdeff1344b52050`. These are local measurements, not vendor benchmark figures.

The table pools the 5.64-second English and 6.01-second Swedish fixtures. Each row has 40 samples per condition, split evenly between the two clips. Values are milliseconds. The p95 is the nearest-rank empirical percentile.

| Backend | Normal median | Normal p95 | Added CPU load median | Added CPU load p95 |
| --- | ---: | ---: | ---: | ---: |
| CPU, eight threads, INT8 ONNX | 141.6 | 144.2 | 469.2 | 500.8 |
| CPU, four threads, INT8 ONNX | 176.6 | 184.6 | 475.3 | 504.1 |
| MLX GPU, BF16 weights | 85.4 | 89.0 | 96.7 | 128.0 |

MLX was 1.66 times faster than the eight-thread CPU configuration under normal desktop load and 4.85 times faster with added CPU load. Its slowest short-clip sample under added load was 203 ms. The CPU configuration's slowest was 518 ms. This demonstrates resistance to the tested CPU contention, not immunity to every kind of load.

The older Core ML NeuralNetwork conversion completed a second 360-sample comparison, including a fresh eight-thread CPU control. Both Core ML choices were slower. Values again pool the two short clips.

| Backend | Normal median | Normal p95 | Added CPU load median | Added CPU load p95 |
| --- | ---: | ---: | ---: | ---: |
| CPU, eight threads, repeated control | 140.4 | 145.8 | 465.9 | 513.9 |
| ONNX Core ML, GPU allowed | 244.9 | 271.1 | 1044.1 | 1154.2 |
| ONNX Core ML, Neural Engine allowed | 201.4 | 211.9 | 536.8 | 563.6 |

The Core ML provider accepted 925 of 2380 encoder nodes across 270 partitions, plus 13 of 39 decoder nodes across four partitions. Other work stayed with ONNX Runtime on the CPU. These node counts do not indicate the fraction of computation accelerated. The labels describe permitted devices, not verified exclusive GPU or Neural Engine execution. The repeated CPU control agreed closely with the first comparison. See [coreml-summary.csv](coreml-summary.csv) and [coreml-raw.jsonl](coreml-raw.jsonl).

The newer MLProgram conversion failed before inference for both compute-unit choices. Apple returned a model execution-plan error with code -14 after the provider accepted 1724 encoder nodes across 270 partitions. A diagnostic run with compute-plan profiling stalled and was terminated. Repeating without profiling produced an explicit model-load failure. This is a compatibility result for this export and OS build, not a latency measurement or evidence that native Core ML models cannot work. [GPU failure log](coreml-gpu-no-profile.log), [Neural Engine failure log](coreml-ane-pilot.log).

Separate `/usr/bin/time -l` runs loaded each model, warmed the three clips, then transcribed each once. Peak macOS memory footprint was 250,561,808 bytes for CPU and 2,998,634,872 bytes for MLX, about 0.25 GB and 3.00 GB. Peak RSS was about 885 MB and 889 MB respectively. RSS alone therefore obscures the MLX memory cost. These are process peaks from one run, including initialization, not steady-state memory or measurements of the complete app. [CPU memory log](cpu8-memory.log), [MLX memory log](mlx-gpu-memory.log), [commands](memory-commands.json).

The first successful MLX pilot took about 0.34 seconds to load already-downloaded weights and 1.92 seconds for its first inference. Subsequent runs benefited from warm caches. This was not a fresh-boot or deliberately cold-cache test. Its timed rows exclude initialization and per-shape warmup. Downloads also happen outside the benchmark. Do not interpret the 85 ms median as cold-start latency.

CPU and MLX produced identical transcripts on the separate English and Swedish clips. The Core ML GPU-allowed path matched them. The Neural-Engine-allowed path omitted the Swedish word "ett". The 11.65-second concatenation exposed a separate language problem. The CPU version began the Swedish section with "Hey, the hare test of dictating posvenska". MLX produced "Hey, det här är a test of dictering på svenska". Neither preserved the Swedish section exactly as it did when transcribing that clip alone. Outputs were stable across repetitions. The Core ML variants also corrupted the Swedish section of the concatenated clip, with slightly different wording from the plain CPU version.

This is one concatenated language transition, not a representative test of natural Swedish and English code-switching. The source audio and timing stayed the same across backends, but the export, weight precision, feature extraction, and decoding implementations differ. An accuracy difference cannot be attributed to the processor alone. All distinct outputs are retained in the transcript files.

The harness preloads PCM and model weights, warms each input shape, then measures feature extraction through final decoded text. MLX synchronizes before and after timing. Both backends omit personal vocabulary boosting and the final number and filler passes. The CPU harness uses the vendored app library, memory-mapped INT8 weights, disabled prepacking, eight inference threads, and the app's user-initiated scheduling class. The four-thread variant changes only the thread count. Core inference dependencies match the app lockfile; the standalone lock resolves tokenizers to 0.23.2 rather than the app's 0.23.1.

Each comparison runs two rounds in reversed backend and condition order. Added load uses 14 compiled Swift busy workers at user-initiated priority, started after model warmup and stopped after each case. Normal load retains the user's existing desktop activity. No other apps were closed or reprioritized. Repeated short fixtures and this sample count do not establish production tail latency.

The benchmark does not measure microphone capture, key-release handling, paste, memory eviction, thermal steady state, competing GPU work, or cloud latency. It did not reproduce the original multi-second stalls. A GPU implementation still needs a larger accuracy set, the existing vocabulary behavior, and release-to-paste measurements under memory pressure before becoming the default. A native Core ML export remains untested and should be assessed separately from ONNX conversion.

The reusable harness and pinned dependencies are in [scripts/bench-backends](../../scripts/bench-backends/README.md). [summary.csv](summary.csv) contains per-clip statistics, [raw.jsonl](raw.jsonl) contains every timing, and [transcripts.json](transcripts.json) contains the first comparison's distinct outputs. [metadata.json](metadata.json) records the machine and command settings. The two comparisons contain 720 timed transcriptions in total. [versions.json](versions.json) pins the model revision, inference versions, and fixture/model hashes. No application code changed. The standalone Rust harness passed formatting and Clippy with warnings treated as errors. The Python scripts passed syntax checks, and all expected sample counts were verified.
