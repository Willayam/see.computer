# Recording compression and upload tests, September 9, 2026

Start with local, hardware-encoded H.264 at 1440p, 15 fps, and a 2 Mbps average bitrate target. Upload completed one-second segments while recording continues. Keep explicit screenshots as PNG and transcribe the original microphone audio locally.

This is a tested starting point for a sharing implementation, not a shipped app feature. All recording content in this experiment is synthetic. No personal screen content or microphone audio was uploaded.

## Compression and transcription

Twenty controlled trials ran two rounds across four encoding profiles and a transcription-only baseline, both under the existing desktop workload and with 14 additional busy CPU workers. Every condition has 32 timed transcriptions across the English and Swedish fixtures, for 320 total.

| Video profile | Size for 12 seconds | Median STT, normal | Median STT, CPU load | OCR word edits versus reference |
| --- | ---: | ---: | ---: | ---: |
| No video pipeline | n/a | 205 ms | 433 ms | n/a |
| 1440p, 15 fps, 2 Mbps target | 2.67 MB | 210 ms | 475 ms | 8/521 |
| 1440p, 15 fps, 4 Mbps target | 3.51 MB | 208 ms | 484 ms | 7/521 |
| 1440p, 30 fps, 4 Mbps target | 3.54 MB | 227 ms | 477 ms | 5/521 |
| 1080p, 15 fps, 2 Mbps target | 2.65 MB | 212 ms | 468 ms | 10/521 |

At the same target bitrate, downscaling to 1080p saved less than 1% on this fixture and increased OCR drift slightly. The 1440p, 15 fps, 2 Mbps profile used about 24% less space than the 4 Mbps profile. Its file size corresponds to about 13.3 MB per minute for this content. Bitrate targets do not guarantee a fixed size or quality on other recordings.

The selected profile added about 5 ms to median transcription under normal conditions and 41 ms under CPU load. Its loaded p95 was 831 ms, versus 533 ms for transcription alone. This remains a small, noisy experiment on a working desktop. It demonstrates that hardware video encoding still has some client overhead. It does not establish a production tail-latency guarantee.

The matrix includes lossless source decoding, resizing where applicable, hardware encoding, and a loopback upload sender and receiver. The app's direct capture path will have a different CPU and memory profile. Model loading and warmup are excluded. No artificial memory pressure was introduced.

## Native capture

A separate ScreenCaptureKit test captured only the benchmark's own window. AVAssetWriter required hardware H.264 encoding and emitted fragmented MP4 data directly. The final verified run used 1440p, 15 fps, a 2 Mbps target, and a three-frame capture queue.

- It accepted 179 frames in the 12-second recording and rejected none at the writer. ScreenCaptureKit can omit unchanged frames, so this is not a claim that every possible capture interval produced a frame.
- The initialization data and first media segment became available about 1.35 seconds after recorder construction. Subsequent segments appeared about once per second.
- The assembled movie is 2.65 MB and 12.03 seconds long, starting at zero. All 179 packet presentation and decoding timestamps increase strictly. The movie decodes successfully.
- Capture timestamps must be rebased before segmented writing. The first prototype inherited the system uptime in the movie duration. The verified output fixes that issue.

The native test process includes the synthetic AppKit window and its large source image. Its resource log therefore does not isolate the incremental cost of adding recording to the app. See [native-capture.log](native-capture.log) for maximum RSS and peak physical footprint, and [native-capture.json](native-capture.json) for segment timing. Do not treat those two memory metrics as interchangeable.

The FFmpeg matrix's 1440p, 15 fps, 2 Mbps process averaged about 141 to 143 MB maximum RSS and 109 to 111 MB peak physical footprint. The 30 fps profile used about 172 to 180 MB maximum RSS. These numbers exclude the separate speech engine and the Python HTTP test process.

## Upload queue behavior

The controlled HTTP test used a disk queue, one upload at a time, and a 64 KiB read buffer. Its receiver verified every completed object by SHA-256.

At a simulated 5 Mbps with 50 ms request overhead, the selected profile's first media object finished uploading about 1.5 to 1.6 seconds after encoding started. The final upload completed about 0.27 seconds after the encoder finished. These timings include the test receiver, not R2.

At 0.5 Mbps, the test returned one HTTP 503 and retried the same object. All 13 objects, initialization plus 12 media segments, arrived intact. The disk backlog peaked at about 2.22 MB. Uploading finished about 33.4 seconds after encoding ended, because the link could not keep up with the recording bitrate. The application should expose that pending state while preserving local recording and transcription.

The fixed read buffer does not mean the whole Python process uses 64 KiB. Socket buffers, file hashes, test metadata, and the receiver allocate memory too. A production queue also needs a disk space limit and restart recovery. This test covers a slow link and one retry, not those recovery cases.

## Text and audio quality

The fixture contains Swedish and English, code, numbers, small text, and scrolling. Three frames per profile were scored with Apple Vision OCR against the corresponding uncompressed reference. The counts measure changes in OCR output, including OCR's own errors. They are not a human-verified text accuracy score. Punctuation and case are ignored. View the samples at their original size before choosing a final preset.

An explicit screenshot was 302 KB as PNG and 249 KB as JPEG at FFmpeg quality level 4. That saves only about 17%, and the OCR output changed. Keeping PNG is the better default for the screenshots agents use to read small details.

Separately, a 5.64-second English fixture fell from 180 KB PCM WAV to 51 KB AAC at a 64 kbps target. This measured size only. It did not evaluate listening quality or synchronized audio/video streaming. Keep original PCM on the local transcription path regardless of the sharing codec.

## Try the results

The experiment is live on R2 in Cocco Ventures.

- [Open the public comparison page](https://see-computer-media-lab.cocco.workers.dev/d/634451b4232419790bc1e96eecb92eed/index.html)
- [Open the native recording uploaded during capture](https://see-computer-media-lab.cocco.workers.dev/d/da8ec64702d8b1ac0785c9ba5aef7195/index.html)
- [Read the hosted recording index as an agent](https://see-computer-media-lab.cocco.workers.dev/d/da8ec64702d8b1ac0785c9ba5aef7195/take.md)

Local copies are also included.

- [1440p, 15 fps, 2 Mbps video](samples/1440p-15-2m.mp4)
- [1440p, 15 fps, 4 Mbps video](samples/1440p-15-4m.mp4)
- [1440p, 30 fps, 4 Mbps video](samples/1440p-30-4m.mp4)
- [1080p, 15 fps, 2 Mbps video](samples/1080p-15-2m.mp4)
- [Direct native capture](samples/native-capture.mp4)
- [Uncompressed reference screenshot](samples/reference-2.png)
- [Compressed 1440p frame](samples/1440p-15-2m-2.png)
- [Compressed frame while scrolling](samples/1440p-15-2m-6.png)

The complete matrix is in [raw.jsonl](raw.jsonl) and [summary.csv](summary.csv). [quality.json](quality.json) includes exact OCR outputs. The [slow-network trace](slow-network.jsonl) retains the failed request and retry. [Reproduction instructions](../../scripts/bench-media/README.md) include the synthetic recorder, encoder, upload test, and isolated R2 Worker.

## Hosting and app updates

R2 was activated on Cocco Ventures with the user's approval. The isolated bucket and Worker were deployed successfully. The static comparison files uploaded in about 0.8 to 1.9 seconds per movie, then passed full public-download SHA-256 checks. Chrome played the 1440p, 15 fps, 2 Mbps movie with a 12-second duration and no video error. A live HTTP check returned the requested 32-byte range with status 206, and a write without the owner token returned 401.

The final native live test uploaded and publicly verified all 13 emitted objects, initialization plus 12 video segments. The first media object was verified 3.71 seconds after process launch, during the 12-second capture. All segment verification finished by 13.83 seconds. Successful PUTs took about 269 to 663 ms per object. This single final run reused one HTTPS connection for uploads and verification. It is not a latency SLA, and it includes read-back requests that a production uploader would not need for every chunk. See [r2-live.json](r2-live.json) and [r2-live-capture.json](r2-live-capture.json).

Earlier attempts encountered local connection failures. The first native R2 pilot also revealed that checking a stale list of files could miss segments created during a slow upload. The publisher now rescans after capture exits and requires every emitted segment to have a verified remote copy. The final run passed that check without a retry. Pilot timings are not pooled into the final result.

The repository's existing site Worker targets Cocco Ventures. Use that account for recording storage too. The experiment has its own `see-computer-media-lab` Worker and bucket configuration; it does not change the existing site routes. The benchmark uploader uses a private owner token and public synthetic assets. Production sharing still needs user authentication, per-recording access, quotas, deletion, retention, and the in-app switch.

The published v0.1.0 release has an Apple Silicon DMG. The existing release workflow builds a signed and notarized DMG when its signing secrets are configured. It does not currently generate update bundles, and the app has no updater plugin.

Use Tauri's updater for the next distribution step. Add its plugin, embed the update public key, enable `createUpdaterArtifacts`, and keep the private update-signing key in release secrets. Publish immutable `.app.tar.gz` bundles and signatures first, then update the static manifest. R2 can host these files, although GitHub Releases already offers a simpler initial host. Apple code signing and notarization remain separate from Tauri's update signature. Download updates in the background and offer installation when no take is active.

This turn did not add auto-update behavior, change the release workflow, or build a new distributable app. The test code and hosting experiment are separate from production application code.

## Validation and sources

Swift compiled with warnings treated as errors. Python scripts passed syntax checks. Worker tests passed 22 assertions for authorization, size limits, missing objects, HEAD, and video byte ranges. All 20 matrix trials completed and all five saved movies decoded. Native packet timestamps and duration were checked independently with FFprobe.

- [Apple hardware video encoding](https://developer.apple.com/videos/play/wwdc2021/10158/)
- [Apple fragmented MP4 writing](https://developer.apple.com/videos/play/wwdc2020/10011/)
- [R2 presigned upload URLs](https://developers.cloudflare.com/r2/api/s3/presigned-urls/)
- [Tauri updater and static manifests](https://v2.tauri.app/plugin/updater/)
- [Published v0.1.0 release](https://github.com/Willayam/see.computer/releases/tag/v0.1.0)
