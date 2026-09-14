# Local media compression and upload tests

These experiments capture a synthetic application window, compare hardware video encoding settings, measure interference with local transcription, and check a disk-backed upload queue. They do not modify the app or capture the microphone or other application windows.

## Prepare the fixture

Requires macOS 14 or later, Swift command-line tools, FFmpeg with VideoToolbox support, Python 3, and the local transcription benchmark executable and prepared model. See [the backend benchmark](../bench-backends/README.md) for the latter. The controlled load test uses its compiled `busy` helper.

```sh
mkdir -p .audit/media-bench
swiftc -O -warnings-as-errors scripts/bench-media/fixture.swift -o .audit/media-bench/fixture
.audit/media-bench/fixture png .audit/media-bench/page.png
ffmpeg -y -hide_banner -loglevel error -loop 1 -framerate 30 \
  -i .audit/media-bench/page.png -t 12 \
  -vf 'crop=2560:1440:0:min(max(t-3\,0)*150\,900),format=yuv420p' \
  -c:v ffv1 -threads 2 .audit/media-bench/source.mkv
```

The source has static text, Swedish and English, code, numbers, and a scrolling interval. It is a lossless video derived from one generated page. It does not represent photographs, games, or camera video.

## Run the matrix

```sh
python3 scripts/bench-media/run.py --rounds 2 --out .audit/media-bench/matrix
python3 scripts/bench-media/summarize.py .audit/media-bench/matrix
python3 scripts/bench-media/run.py --slow-only --out .audit/media-bench/slow-network
python3 scripts/bench-media/quality.py
```

Use new output directories for each run. The matrix plays the lossless source at approximately recording speed, encodes H.264 with software fallback disabled, and emits one-second fragmented MP4 segments. FFmpeg uses two decoder threads and one filter thread. Its normal read-rate scheduling can release an initial fraction of a second early, so its process duration is not an exact microphone capture clock.

The four profiles compare 1440p at 15 fps and 2 Mbps, 1440p at 15 fps and 4 Mbps, 1440p at 30 fps and 4 Mbps, and 1080p at 15 fps and 2 Mbps. Average bitrate is a target, not a strict size cap. Each condition has two rounds, eight repetitions per English and Swedish fixture per round, and 32 timed transcriptions total. Trials run sequentially in shuffled order with seed `731 + round`.

The baseline runs transcription alone. CPU-load conditions briefly start 14 busy workers, then stop those workers after the trial. This is CPU contention, not a controlled memory-pressure test. Existing desktop activity remains present. Model loading and warmup happen before timing. Memory numbers for FFmpeg include decoding and resizing the fixture, as well as encoding; they exclude the transcription process and test upload receiver.

The upload test uses loopback HTTP. A sender reads one file at a time in 64 KiB blocks and limits its rate to 5 Mbps, with 50 ms of simulated request overhead. Files remain on disk. The slow test uses 0.5 Mbps and returns one HTTP 503 to exercise a retry. Every successful received file must match its source SHA-256. These are controlled queue tests, not measured R2 latency. They do not test process restart recovery, lost connectivity for hours, or disk exhaustion.

`quality.py` extracts frames at 2, 6, and 10 seconds and runs Apple's Vision OCR with language correction disabled. It compares OCR words with OCR of the uncompressed reference frame. This measures recognition drift, not human accuracy. Inspect the PNGs and movies too. The screenshot comparison uses FFmpeg JPEG quality level 4, not a percentage quality value.

## Native recording and segmentation

```sh
/usr/bin/time -l .audit/media-bench/fixture capture-segments \
  .audit/media-bench/native-recording 12 \
  > .audit/media-bench/native-recording.json \
  2> .audit/media-bench/native-recording.log
```

Keep `page.png` in the parent directory of the output directory. The recorder opens its own synthetic window and captures only that window through ScreenCaptureKit. Screen Recording permission is required. It uses a three-frame capture queue, 1440p at 15 fps, hardware-required H.264 at a 2 Mbps target, and AVAssetWriter segment callbacks. Segment files are atomically written, so an uploader can read completed objects while recording continues. This path has no FFmpeg dependency.

Capture timestamps are rebased to zero before entering the writer. Without this step, concatenated segments can have a duration based on system uptime. The test records segment availability times and rejected frames. It captures no audio. `capture OUTPUT.mov 12` exercises a conventional 1440p, 30 fps, 4 Mbps hardware movie writer instead.

## R2 experiment

The isolated Worker configuration targets Cocco Ventures, the same Cloudflare account as see.computer. R2 must first be enabled on that account. It uses a separate `see-computer-media-lab` bucket and Worker. It does not modify the existing site or its routes.

```sh
CLOUDFLARE_ACCOUNT_ID=29392a6d23b11e67789d7f1c9bcc5853 bunx wrangler r2 bucket create see-computer-media-lab
bunx wrangler deploy --config scripts/bench-media/worker/wrangler.toml
```

Generate a private upload token in a file outside Git, store it as the Worker's `BENCH_UPLOAD_TOKEN` secret, and pass that file to the publisher. Do not put the token in a URL or frontend code. The Worker accepts owner-authorized PUTs up to 8 MiB and serves public reads under random experiment identifiers. It supports video byte ranges. Every asset published by this script is synthetic test content. This owner-only uploader is not the app's production account or quota system.

```sh
bunx wrangler secret put BENCH_UPLOAD_TOKEN --config scripts/bench-media/worker/wrangler.toml < /private/path/upload-token
python3 scripts/bench-media/publish.py --endpoint https://YOUR-WORKER.workers.dev --token-file /private/path/upload-token
python3 scripts/bench-media/publish.py --endpoint https://YOUR-WORKER.workers.dev --token-file /private/path/upload-token --live
```

The first command publishes comparison movies and a screenshot. The live command captures a new synthetic window, uploads each segment as it appears, and verifies public reads by hash. Each writes a `take.md` index and local upload measurements. Never interpret the prepared command as evidence that deployment or R2 verification already succeeded.

## Checks

```sh
python3 -m py_compile scripts/bench-media/*.py
swiftc -O -warnings-as-errors scripts/bench-media/fixture.swift -o /tmp/see-media-fixture-check
bun test scripts/bench-media/worker/worker.test.mjs
```

The Worker tests exercise authorization, object size validation, missing objects, HEAD, and byte ranges against a small in-memory R2 substitute. A successful local test does not establish that Cloudflare configuration or billing is ready.
