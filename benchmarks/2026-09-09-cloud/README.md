# Cloud transcription comparison, September 9, 2026

AssemblyAI is the leading cloud candidate in this small test. It preserved both languages with no word errors on the four fixtures and had the lowest median time from recording stop to final text. That does not establish a production winner. Soniox was an initial candidate based on its documented capabilities, not measured superiority, and could not be tested without credentials.

For the current product direction, keep local transcription and build immediate media upload separately. An agent should be able to open uploaded screenshots and clips while the local transcript is still processing. Cloud transcription can remain an optional fallback.

## Results

Five rounds across English, Swedish, Swedish followed by English, and English followed by Swedish. Each provider had 20 attempts. Timings and word error rates include successful requests only. All failures remain in the raw data.

| Provider and model | Input mode | Completed | Median after stop | p95 after stop | Word errors / reference words |
| --- | --- | ---: | ---: | ---: | ---: |
| AssemblyAI Universal-3.5 Pro | Streaming | 20/20 | 197 ms | 1,534 ms | 0/525 |
| ElevenLabs Scribe v2 Realtime | Streaming | 17/20 | 283 ms | 592 ms | 5/438 |
| Groq Whisper large-v3-turbo | File after stop | 15/20 | 333 ms | 4,603 ms | 72/421 |
| Groq Whisper large-v3 | File after stop | 17/20 | 611 ms | 3,748 ms | 8/437 |

AssemblyAI matched the reference words in every completed trial. ElevenLabs consistently wrote `dictering` instead of `diktering` on the Swedish-first mixed clip. Groq large-v3 omitted `ett` on mixed clips. Groq turbo translated the English half of the Swedish-first clip into faulty Swedish in all four completed attempts on that clip. Its fast median therefore does not make it suitable for this mixed-language setting.

There are only two source recordings, each about six seconds long. The mixed clips concatenate them in both directions. These are repetitions of simple fixtures, not hundreds of distinct spoken examples. Natural switching within a sentence, names, numbers, hesitation, noise, and cleanup remain untested. Case and punctuation do not affect the word score. Missing requests also change the mix of words in each provider's aggregate score. Use the per-clip rows in [summary.csv](summary.csv) and exact outputs in [transcripts.json](transcripts.json).

## Connection behavior

| Streaming provider | Median setup | p95 setup | Median first text from start | Median commit to final |
| --- | ---: | ---: | ---: | ---: |
| AssemblyAI | 835 ms | 6,904 ms | 1,026 ms | 195 ms |
| ElevenLabs | 209 ms | 3,218 ms | 2,317 ms | 281 ms |

AssemblyAI's slowest setup was about 8.7 seconds. That exceeded the duration of the English fixture and left a backlog after recording stopped. The resulting final transcript arrived about 3.6 seconds after stop. Measuring only finalization would hide that delay.

ElevenLabs had three connection errors and Groq had eight across both models. ElevenLabs reported macOS error 51, network unreachable. Groq reported failed connection attempts. Separate pilot checks also saw intermittent connection failures with curl. These observations do not isolate a provider outage or establish a provider reliability ranking. Repeat on a stable connection before using the tails or failure rates for a service decision. Failed requests have no final-transcript latency and are not included in p95.

## Method and scope

The test ran on the same Mac as the local backend benchmark, an M3 Max with 14 CPU cores and 36 GiB RAM. It did not introduce CPU or memory pressure. Each trial opened a new connection. Streaming started connection setup at simulated recording start, then sent 100 ms PCM blocks as samples became available. Startup audio buffered until the socket was ready. Ending the recording explicitly requested final text. No cleanup model ran.

AssemblyAI used its EU endpoint, `universal-3-5-pro`, `min_latency`, and Swedish and English language codes. ElevenLabs used `scribe_v2_realtime`, manual commit, and Swedish and English secondary language hints. Live session acknowledgements confirmed the model and mode settings. Groq used its transcription file endpoint, temperature zero, and the prompt `Swedish and English.` Its timings include upload after recording, so it is a batch baseline.

The script excludes token issuance, microphone capture, app insertion, video upload, reconnect recovery, and cleanup. It does not benchmark a complete desktop integration. It sends only repository fixtures, and uses the available billable provider credentials. Pilot runs were excluded from these results. Soniox lacked credentials; Deepgram, Speechmatics, and OpenAI were also unavailable and were not tested.

Reproduction instructions and metric definitions are in [the benchmark README](../../scripts/bench-cloud/README.md). The artifact includes [raw attempts](raw.jsonl), [metadata](metadata.json), [fixture references and hashes](fixtures.json), and per-request server events. It does not contain credentials or copies of provider documentation.

Validation checked Python syntax, word scoring, percentile calculation, failure accounting, all 80 unique provider/clip/round combinations, streaming timestamp ordering, and absence of the configured API keys in saved output. No application code changed.

## Next decision

Build the shareable recording flow first. Create the URL when recording starts, upload media incrementally with a bounded memory buffer, and attach the local transcript when it finishes. Expose an agent-readable index with timestamps and direct links to the available assets. Media readiness should not wait on transcription.

The existing app packages these assets locally, but its recorder uses macOS `screencapture` to write a `.mov`. First verify a recording format and upload path that can send durable data before recording stops. Do not assume that reading a growing movie file produces a complete or playable remote recording. Keep queued data on disk when the connection falls behind.

If cloud transcription becomes a priority, compare AssemblyAI and ElevenLabs on a larger set of natural Swedish-English dictation, then add Soniox and other providers when credentials are available. Include CPU and memory pressure in the client test. The current result is enough to reject choosing Soniox without measurement and to identify the language-switching problem with Groq turbo under these settings.

## Sources

Provider capabilities and request settings were checked against current primary documentation on September 9, 2026.

- [AssemblyAI streaming models and language support](https://www.assemblyai.com/docs/streaming/select-the-speech-model)
- [AssemblyAI WebSocket API](https://www.assemblyai.com/docs/streaming/api-spec/streaming-websocket)
- [ElevenLabs realtime API](https://elevenlabs.io/docs/api-reference/speech-to-text/v-1-speech-to-text-realtime)
- [Groq speech to text API](https://console.groq.com/docs/speech-to-text)
