# Cloud transcription benchmark

Compare ElevenLabs Scribe v2 Realtime and AssemblyAI Universal-3.5 Pro streaming with Groq Whisper large-v3 and large-v3-turbo file uploads. An optional Soniox adapter exists but has not been tested against the live API.

This script sends the repository's English and Swedish fixtures to the selected providers. Calls use billable APIs. It does not record the microphone or change the app.

## Run

```sh
uv venv --python 3.12 /tmp/see-cloud-bench-venv
uv pip install --python /tmp/see-cloud-bench-venv/bin/python -r scripts/bench-cloud/requirements.txt
/tmp/see-cloud-bench-venv/bin/python scripts/bench-cloud/run.py --rounds 5 --out .audit/cloud-bench/results
/tmp/see-cloud-bench-venv/bin/python scripts/bench-cloud/summarize.py .audit/cloud-bench/results
```

Read credentials from `ELEVENLABS_API_KEY`, `ASSEMBLYAI_API_KEY`, `GROQ_API_KEY`, and optionally `SONIOX_API_KEY`. `--env-file /absolute/path/to/private.env` loads these names from a simple dotenv file. Missing credentials skip that provider and appear in metadata. To include Soniox, explicitly add it to `--providers`.

Use a new output directory for each run. The script overwrites an existing run. `--clips en sv sv-en en-sv` selects clips and `--providers elevenlabs assemblyai groq-turbo groq-large` selects providers. Results include sanitized server events, transcripts, errors, fixture hashes, model names, and connection settings. Outgoing audio and credentials are not written to event logs.

## Method

Each round shuffles provider and clip order with a recorded deterministic seed rule of `420 + round`. Requests run sequentially. Every trial opens a new connection. No retries hide failed attempts. Two authentication or quota errors stop further requests for that provider.

Streaming trials simulate recording from the time connection setup begins. They send mono 16 kHz PCM in 100 ms blocks as the samples become available. Audio that accumulates during connection setup is sent immediately once the connection is ready. The final block may be up to 150 ms to avoid a tiny remainder.

ElevenLabs uses manual commit with Swedish and English secondary language hints. AssemblyAI uses its EU endpoint, both language codes, and `min_latency` mode. A 10-second silence threshold keeps automatic turn detection from ending these short fixtures. Releasing the simulated recording forces the final transcript. These settings target push-to-talk dictation.

Groq accepts complete files in this test. Its request begins after the simulated recording ends. It is a batch baseline, not a streaming implementation. The request sets temperature zero and a short Swedish and English prompt.

The optional Soniox adapter uses language hints and explicit finalization after 200 ms of silence. That delay counts toward stop latency. Validate this adapter with a pilot before comparing its results.

## Metrics

- `stop_to_final_ms` measures from the last captured sample to receipt of final text. It includes any remaining upload backlog. Groq includes HTTP client creation, connection, upload, and response parsing.
- `commit_to_final_ms` measures the streaming finalization phase after sending captured audio. Soniox includes the extra silence before its finalize message.
- `setup_ms` includes the WebSocket handshake and provider session acknowledgement. Soniox has no acknowledgement wait in this adapter.
- `first_audio_ms` and `first_text_ms` start at simulated recording start.
- `pacing_max_late_ms` includes startup buffering. It is not a measure of CPU scheduling jitter alone.
- Word error rate uses Levenshtein distance over case-insensitive Unicode words. Punctuation is ignored. References and exact outputs are saved for inspection.

The summary reports median and nearest-rank p95 over successful requests only. Failures remain in separate counts. With 20 attempts per provider, p95 is a descriptive result from this run, not a production latency estimate. Pooled quality can have uneven clip weights when requests fail, so inspect per-clip results.

Only two short source recordings are used. Concatenating them checks language transitions in both directions but does not represent natural switching within a sentence. This test does not measure heavy memory pressure, microphone capture, temporary-token issuance, app text insertion, cleanup, or reconnect recovery. It uses no cleanup model. A client that reuses connections will have different setup costs.

## Provider references

- [AssemblyAI model selection](https://www.assemblyai.com/docs/streaming/select-the-speech-model)
- [AssemblyAI WebSocket API](https://www.assemblyai.com/docs/streaming/api-spec/streaming-websocket)
- [AssemblyAI manual turn detection](https://www.assemblyai.com/docs/streaming/turn-detection)
- [ElevenLabs realtime API](https://elevenlabs.io/docs/api-reference/speech-to-text/v-1-speech-to-text-realtime)
- [Groq speech to text API](https://console.groq.com/docs/speech-to-text)
- [Soniox manual finalization](https://soniox.com/docs/stt/rt/manual-finalization)
