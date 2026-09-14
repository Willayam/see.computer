# Sharing beta verification

Version 0.1.1 was built on September 9, 2026. The local DMG is at `src-tauri/target/release/bundle/dmg/see.computer_0.1.1_aarch64.dmg`.

## What changed

Sharing is off by default. The setting applies to new takes. Plain dictation stays local. Captures use a private installation credential and a durable upload queue. Cancelling a take queues remote deletion. Pausing sharing stops new uploads and preserves pending work for later.

ScreenCaptureKit supplies frames to hardware H.264 encoding at up to 1440p, 15 fps and 2 Mbps. Mono microphone audio uses 64 kbps AAC. Completed fragments are written atomically, then uploaded by a utility thread. A static screen repeats its last frame so fragments continue arriving. Uploaded screenshots are resized to a maximum dimension of 2560 pixels and encoded as JPEG; local PNG originals remain intact.

The share page supports Safari's native HLS player and Chrome through a self-hosted hls.js player. It exposes the transcript and a Markdown index with absolute media links for agents.

Signed updates download in the background. Verified archives wait in a private temporary file, keeping their bytes out of RAM. Installation requires the user to choose Restart to update while the session controller is idle.

## Verification

- Rust formatting and Clippy passed with warnings denied.
- 156 Rust tests passed. The live R2 test is excluded from the default suite and was run separately.
- 21 sharing service tests passed, covering ownership, quotas, retries, cancellation, expiry, path restrictions and byte ranges.
- The native synthetic media test produced five fragments from one static image and generated audio. The video lasted 4.06 seconds and AAC audio lasted 4.05 seconds.
- The final R2 integration test uploaded its first media fragment after 2,520 ms, before recording stopped. Publishing finished after 7,491 ms.
- Chrome played the synthetic share through to the end. [Final test recording](https://see-computer-shares.cocco.workers.dev/s/f1c953e91ed02fedaedb3d345e45c6b8/).
- The release binary transcribed the English fixture in 160 ms, excluding model loading. Local take packaging extracted audio and two JPEG frames from the new fragmented movie format.
- The app passed macOS code-signature verification. The updater signature matched the embedded public key. A modified archive failed verification.

No personal recording was uploaded during these checks. The native test generates its image and audio in memory. The permission-gated ScreenCaptureKit and microphone startup flow still needs a manual check in the beta. The installed app was already running and was left intact.

## Deployment and release status

The sharing Worker and private R2 bucket are named `see-computer-shares` in Cocco Ventures. The Worker is live at https://see-computer-shares.cocco.workers.dev. Links expire after 30 days; the R2 lifecycle rule deletes each object 30 days after its upload.

The beta allows 10 creations per installation per UTC month, including cancelled shares. Each share permits 256 MiB and 1,500 files, with an 8 MiB object limit. Daily capacity is 100 creations across the beta and 20 per IP address. There is no paid plan or per-person account enforcement yet.

The local DMG uses the available Apple Development certificate. It is not notarized. A public release needs the Developer ID Application certificate and Apple notarization secrets described in [Signed app updates](updates.md). The updater private key is stored outside the repository and in GitHub Actions secrets. Version 0.1.0 needs a manual installation of the first updater-enabled release.

The release workflow produces a draft. No app release was published, and the existing website deployment was not changed.
