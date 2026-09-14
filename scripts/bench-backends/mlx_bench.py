"""Same v3 model through MLX. Load audio outside the timer, compute features inside."""
import ctypes
import json
import sys
import time
import wave

import mlx.core as mx
import numpy as np
from parakeet_mlx import from_pretrained
from parakeet_mlx.audio import get_logmel

ctypes.CDLL(None).pthread_set_qos_class_self_np(0x19, 0)
mx.set_default_device(mx.gpu)
model_path, runs, *paths = sys.argv[1:]


def emit(**record):
    print(json.dumps(dict(mode="mlx-gpu", **record)), flush=True)


start = time.perf_counter()
model = from_pretrained(model_path)
mx.eval(model.parameters())
mx.synchronize()
emit(event="load", ms=(time.perf_counter() - start) * 1000)
clips = []
for path in paths:
    with wave.open(path) as wav:
        assert (wav.getframerate(), wav.getnchannels(), wav.getsampwidth()) == (16000, 1, 2)
        samples = np.frombuffer(wav.readframes(wav.getnframes()), dtype=np.int16).astype(np.float32) / 32768
    clips.append((path, samples))


def transcribe(path, samples, event, run=-1):
    mx.synchronize()
    start = time.perf_counter()
    # Match parakeet_mlx.audio.load_audio, which returns float32 PCM.
    audio = mx.array(samples, dtype=mx.float32)
    result = model.generate(get_logmel(audio, model.preprocessor_config))[0]
    mx.synchronize()
    elapsed = (time.perf_counter() - start) * 1000
    emit(event=event, clip=path, run=run, seconds=len(samples)/16000, ms=elapsed, text=result.text)


for path, samples in clips:
    transcribe(path, samples, "warm")
emit(event="ready", device=str(mx.default_device()))
sys.stdin.readline()
for run in range(int(runs)):
    for path, samples in clips:
        transcribe(path, samples, "sample", run)
emit(event="memory", peak_gpu_bytes=mx.get_peak_memory())
