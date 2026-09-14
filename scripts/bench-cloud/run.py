"""Compare cloud STT with real-time paced PCM and explicit end-of-recording signals."""
import argparse
import asyncio
import base64
import hashlib
import json
import os
from pathlib import Path
import random
import re
import sys
import time
from urllib.parse import urlencode
import wave

import httpx
from websockets.asyncio.client import connect

ROOT = Path(__file__).resolve().parents[2]
KEYS = {"elevenlabs": "ELEVENLABS_API_KEY", "assemblyai": "ASSEMBLYAI_API_KEY",
        "groq-turbo": "GROQ_API_KEY", "groq-large": "GROQ_API_KEY", "soniox": "SONIOX_API_KEY"}
MODELS = {"elevenlabs": "scribe_v2_realtime", "assemblyai": "universal-3-5-pro",
          "groq-turbo": "whisper-large-v3-turbo", "groq-large": "whisper-large-v3", "soniox": "stt-rt-v5"}
REFERENCE = {
    "en": "Hi, this is a quick dictation test in English. We are measuring how fast the text shows up.",
    "sv": "Hej! Det här är ett test av diktering på svenska. Vi mäter hur snabbt texten dyker upp.",
}


def safe(value):
    text = json.dumps(value, ensure_ascii=False)
    for key in set(KEYS.values()):
        if os.environ.get(key):
            text = text.replace(os.environ[key], "[REDACTED]")
    return json.loads(text)


def words(text):
    return re.findall(r"\w+", text.casefold())


def edit_distance(expected, actual):
    previous = list(range(len(actual) + 1))
    for i, a in enumerate(expected, 1):
        current = [i]
        for j, b in enumerate(actual, 1):
            current.append(min(current[-1] + 1, previous[j] + 1, previous[j-1] + (a != b)))
        previous = current
    return previous[-1]


def load_clips(out):
    pcm = {}
    for name in ["en", "sv"]:
        with wave.open(str(ROOT / "fixtures" / f"{name}.wav")) as wav:
            assert (wav.getframerate(), wav.getnchannels(), wav.getsampwidth()) == (16000, 1, 2)
            pcm[name] = wav.readframes(wav.getnframes())
    pcm["sv-en"] = pcm["sv"] + pcm["en"]
    pcm["en-sv"] = pcm["en"] + pcm["sv"]
    references = dict(REFERENCE)
    references["sv-en"] = REFERENCE["sv"] + " " + REFERENCE["en"]
    references["en-sv"] = REFERENCE["en"] + " " + REFERENCE["sv"]
    clips = {}
    for name, audio in pcm.items():
        path = out / f"{name}.wav"
        with wave.open(str(path), "wb") as wav:
            wav.setparams((1, 2, 16000, 0, "NONE", "not compressed"))
            wav.writeframes(audio)
        clips[name] = dict(pcm=audio, wav=path.read_bytes(), reference=references[name], seconds=len(audio)/32000)
    (out / "fixtures.json").write_text(json.dumps({name: {
        "reference": clip["reference"], "seconds": clip["seconds"],
        "pcm_sha256": hashlib.sha256(clip["pcm"]).hexdigest(),
    } for name, clip in clips.items()}, indent=2, ensure_ascii=False) + "\n")
    return clips


async def streaming(provider, clip, events):
    key = os.environ[KEYS[provider]]
    if provider == "elevenlabs":
        endpoint = "wss://api.elevenlabs.io/v1/speech-to-text/realtime"
        params = {"model_id": MODELS[provider], "audio_format": "pcm_16000", "commit_strategy": "manual",
                  "secondary_languages": ["sv", "en"]}
        headers = {"xi-api-key": key}
    elif provider == "assemblyai":
        endpoint = "wss://streaming.eu.assemblyai.com/v3/ws"
        params = {"sample_rate": 16000, "speech_model": MODELS[provider], "encoding": "pcm_s16le",
                  "language_codes": json.dumps(["sv", "en"]), "mode": "min_latency",
                  "min_turn_silence": 10000, "max_turn_silence": 10000}
        headers = {"Authorization": key}
    else:
        endpoint = "wss://stt-rt.soniox.com/transcribe-websocket"
        params = {}
        headers = {}
    started = time.perf_counter()
    first_text = None
    first_audio = None
    final_at = None
    commit_at = None
    last_audio = None
    pacing_late = 0
    text = ""
    errors = []
    done = asyncio.Event()
    config = None
    url = endpoint + ("?" + urlencode(params, doseq=True) if params else "")

    def event(message):
        events.append({"ms": (time.perf_counter()-started)*1000, "message": safe(message)})

    async with connect(url, additional_headers=headers, open_timeout=15, close_timeout=3, max_size=4_000_000) as ws:
        if provider == "soniox":
            await ws.send(json.dumps({"api_key": key, "model": MODELS[provider], "audio_format": "pcm_s16le",
                                      "sample_rate": 16000, "num_channels": 1, "language_hints": ["sv", "en"],
                                      "enable_endpoint_detection": False}))
        else:
            initial = json.loads(await asyncio.wait_for(ws.recv(), 15))
            event(initial)
            if initial.get("message_type", initial.get("type")) not in ["session_started", "Begin"]:
                raise RuntimeError(json.dumps(safe(initial)))
            config = initial.get("config", initial.get("configuration"))
        ready_at = time.perf_counter()

        async def receive():
            nonlocal first_text, final_at, text
            final_tokens = []
            try:
                async for raw in ws:
                    message = json.loads(raw)
                    event(message)
                    now = time.perf_counter()
                    kind = message.get("message_type", message.get("type", ""))
                    candidate = message.get("text", message.get("transcript", ""))
                    if candidate and first_text is None:
                        first_text = now
                    if provider == "elevenlabs" and kind == "committed_transcript":
                        text += (" " if text else "") + candidate
                        if commit_at is not None:
                            final_at = now
                            done.set()
                    elif provider == "assemblyai" and kind == "Turn" and message.get("end_of_turn"):
                        text += (" " if text else "") + candidate
                        if commit_at is not None:
                            final_at = now
                            done.set()
                    elif provider == "soniox":
                        tokens = message.get("tokens", [])
                        if any(t.get("text", "").strip() for t in tokens) and first_text is None:
                            first_text = now
                        for token in tokens:
                            if token.get("is_final") and token.get("text") not in ["<fin>", "<end>"]:
                                final_tokens.append(token.get("text", ""))
                        text = "".join(final_tokens)
                        if any(t.get("text") == "<fin>" for t in tokens) and commit_at is not None:
                            final_at = now
                            done.set()
                    if message.get("error") or message.get("error_code") or kind.endswith("error") or kind in ["quota_exceeded", "rate_limited"]:
                        errors.append(safe(message))
                        done.set()
            except Exception as error:
                errors.append(safe({"error": str(error)}))
                done.set()

        reader = asyncio.create_task(receive())
        try:
            pcm = clip["pcm"]
            at = 0
            while at < len(pcm):
                end = min(at + 3200, len(pcm))
                if 0 < len(pcm) - end < 1600:
                    end = len(pcm)
                deadline = started + end/32000
                await asyncio.sleep(max(0, deadline - time.perf_counter()))
                pacing_late = max(pacing_late, time.perf_counter()-deadline)
                chunk = pcm[at:end]
                if provider == "elevenlabs":
                    await ws.send(json.dumps({"message_type": "input_audio_chunk", "audio_base_64": base64.b64encode(chunk).decode(),
                                              "sample_rate": 16000, "commit": False}))
                else:
                    await ws.send(chunk)
                last_audio = time.perf_counter()
                first_audio = first_audio or last_audio
                at = end
            commit_at = time.perf_counter()
            if provider == "elevenlabs":
                await ws.send(json.dumps({"message_type": "input_audio_chunk", "audio_base_64": "", "sample_rate": 16000, "commit": True}))
            elif provider == "assemblyai":
                await ws.send(json.dumps({"type": "ForceEndpoint"}))
            else:
                # Soniox recommends a short silence before finalization. Count it in stop latency.
                for _ in range(2):
                    await asyncio.sleep(.1)
                    await ws.send(bytes(3200))
                await ws.send(json.dumps({"type": "finalize"}))
            await asyncio.wait_for(done.wait(), 20)
            if errors:
                raise RuntimeError(json.dumps(errors))
            if final_at is None:
                raise RuntimeError("no final transcript")
            if provider == "assemblyai":
                await ws.send(json.dumps({"type": "Terminate"}))
        finally:
            reader.cancel()
            await asyncio.gather(reader, return_exceptions=True)
    return dict(kind="streaming", endpoint=endpoint, params=params, server_config=config, text=text.strip(),
                setup_ms=(ready_at-started)*1000, first_audio_ms=(first_audio-started)*1000,
                first_text_ms=(first_text-started)*1000 if first_text else None,
                stop_to_final_ms=(final_at-started-clip["seconds"])*1000,
                commit_to_final_ms=(final_at-commit_at)*1000,
                last_audio_to_final_ms=(final_at-last_audio)*1000, pacing_max_late_ms=pacing_late*1000)


async def batch(provider, clip):
    endpoint = "https://api.groq.com/openai/v1/audio/transcriptions"
    started = time.perf_counter()
    async with httpx.AsyncClient(timeout=30) as client:
        response = await client.post(endpoint, headers={"Authorization": "Bearer " + os.environ[KEYS[provider]]},
                                     files={"file": ("fixture.wav", clip["wav"], "audio/wav")},
                                     data={"model": MODELS[provider], "response_format": "json", "temperature": "0",
                                           "prompt": "Swedish and English."})
        if response.status_code != 200:
            raise RuntimeError(f"HTTP {response.status_code}: {response.text[:1000]}")
        result = response.json()
        final_at = time.perf_counter()
    return dict(kind="batch-after-stop", endpoint=endpoint, text=result["text"].strip(),
                stop_to_final_ms=(final_at-started)*1000, first_text_ms=None,
                setup_ms=None, first_audio_ms=None)


async def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--providers", nargs="+", choices=KEYS, default=["elevenlabs", "assemblyai", "groq-turbo", "groq-large"])
    parser.add_argument("--rounds", type=int, default=5)
    parser.add_argument("--clips", nargs="+", default=["en", "sv", "sv-en", "en-sv"])
    parser.add_argument("--out", type=Path, default=ROOT / ".audit/cloud-bench/results")
    parser.add_argument("--env-file", type=Path)
    args = parser.parse_args()
    if args.env_file:
        for line in args.env_file.read_text().splitlines():
            key, separator, value = line.strip().removeprefix("export ").partition("=")
            if separator and key in set(KEYS.values()):
                os.environ[key] = value.strip().strip("\"'")
    args.out.mkdir(parents=True, exist_ok=True)
    clips = load_clips(args.out)
    available = [p for p in args.providers if os.environ.get(KEYS[p])]
    missing = [p for p in args.providers if p not in available]
    metadata = dict(started=time.strftime("%Y-%m-%dT%H:%M:%S%z"), rounds=args.rounds, providers=available,
                    missing_credentials=missing, models=MODELS, clips=args.clips, chunk_ms=100,
                    python=sys.version, note="New connection at simulated recording start; no auth-token minting overhead.")
    (args.out / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
    failures = dict.fromkeys(available, 0)
    with (args.out / "raw.jsonl").open("w") as out:
        for round_id in range(args.rounds):
            order = [(p, name) for name in args.clips for p in available]
            random.Random(420 + round_id).shuffle(order)
            for provider, name in order:
                if failures[provider] >= 2:
                    continue
                events = []
                record = dict(provider=provider, model=MODELS[provider], clip=name, round=round_id,
                              audio_seconds=clips[name]["seconds"], started=time.strftime("%Y-%m-%dT%H:%M:%S%z"))
                try:
                    operation = batch(provider, clips[name]) if provider.startswith("groq-") else streaming(provider, clips[name], events)
                    record.update(await asyncio.wait_for(operation, 60))
                    reference = words(clips[name]["reference"])
                    record["word_errors"] = edit_distance(reference, words(record["text"]))
                    record["reference_words"] = len(reference)
                    record["wer"] = record["word_errors"] / len(reference)
                    print(f"{round_id} {provider} {name}: {record['stop_to_final_ms']:.0f} ms after stop, {record['word_errors']} word errors", flush=True)
                except Exception as error:
                    record["error"] = safe(str(error) or type(error).__name__)
                    # Stop repeated auth/quota failures. Keep transient network failures in the results.
                    if any(term in record["error"].lower() for term in ["401", "403", "quota", "invalid_api_key", "unauthorized"]):
                        failures[provider] += 1
                    print(f"{round_id} {provider} {name}: {record['error']}", flush=True)
                (args.out / f"{round_id}-{provider}-{name}.events.json").write_text(json.dumps(events, ensure_ascii=False, indent=2) + "\n")
                out.write(json.dumps(safe(record), ensure_ascii=False) + "\n")
                out.flush()
                await asyncio.sleep(.3)


if __name__ == "__main__":
    asyncio.run(main())
