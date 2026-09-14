"""Run backends serially, with normal load and controlled CPU contention.

Example: python3 scripts/bench-backends/run.py --python /tmp/see-backend-bench-venv/bin/python
"""
import argparse
import json
import os
from pathlib import Path
import selectors
import subprocess
import time
import wave

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--python", required=True)
parser.add_argument("--mlx-model", default="mlx-community/parakeet-tdt-0.6b-v3")
parser.add_argument("--modes", nargs="+", default=["cpu8", "cpu4", "mlx-gpu"])
parser.add_argument("--runs", type=int, default=10)
parser.add_argument("--rounds", type=int, default=2)
parser.add_argument("--hogs", type=int, default=14)
parser.add_argument("--timeout", type=int, default=240)
parser.add_argument("--out", type=Path, default=ROOT / ".audit/backend-bench/results")
args = parser.parse_args()
args.out = args.out.resolve()
args.out.mkdir(parents=True, exist_ok=True)
model = Path.home() / "Library/Application Support/see.computer/models/parakeet-tdt-0.6b-v3-onnx/int8-prepared"
clips = [ROOT / "fixtures/en.wav", ROOT / "fixtures/sv.wav", args.out / "mixed.wav"]
with wave.open(str(clips[2]), "wb") as mixed:
    mixed.setparams((1, 2, 16000, 0, "NONE", "not compressed"))
    for path in reversed(clips[:2]):
        with wave.open(str(path)) as wav:
            mixed.writeframes(wav.readframes(wav.getnframes()))
busy_bin = args.out / "busy"
subprocess.run(["swiftc", "-O", str(ROOT / "scripts/busy.swift"), "-o", str(busy_bin)], check=True)
metadata = {"args": vars(args) | {"out": str(args.out)}, "started": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
            "coreml_format": "NeuralNetwork" if os.environ.get("SEE_BENCH_COREML_LEGACY") is not None else "MLProgram",
            "commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
            "macos": subprocess.check_output(["sw_vers"], text=True),
            "hardware": subprocess.check_output(["sysctl", "machdep.cpu.brand_string", "hw.memsize", "hw.logicalcpu"], text=True)}
(args.out / "metadata.json").write_text(json.dumps(metadata, indent=2))


def stop(proc):
    if proc is not None and proc.poll() is None:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()


def case(mode, condition, round_id, output):
    command = ([args.python, str(HERE / "mlx_bench.py"), args.mlx_model, str(args.runs)]
               if mode == "mlx-gpu" else
               [str(HERE / "target/release/see-backend-bench"), mode, str(model), str(args.runs), str(args.out / "coreml-cache")])
    command += [str(path) for path in clips]
    hog = proc = None
    measured = 0
    started = time.monotonic()
    log_path = args.out / f"{round_id}-{condition}-{mode}.log"
    try:
        with log_path.open("w") as log, selectors.DefaultSelector() as selector:
            proc = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log, bufsize=0)
            selector.register(proc.stdout, selectors.EVENT_READ)
            pending = b""
            while True:
                if time.monotonic() - started > args.timeout:
                    raise TimeoutError(f"exceeded {args.timeout}s")
                events = selector.select(timeout=1)
                if not events:
                    if proc.poll() is not None:
                        break
                    continue
                chunk = os.read(proc.stdout.fileno(), 65536)
                if not chunk:
                    break
                pending += chunk
                while b"\n" in pending:
                    line, pending = pending.split(b"\n", 1)
                    record = json.loads(line)
                    record.update(condition=condition, round=round_id)
                    if "clip" in record:
                        record["clip"] = Path(record["clip"]).name
                    output.write(json.dumps(record) + "\n")
                    output.flush()
                    if record["event"] == "ready":
                        if condition == "cpu-load":
                            hog = subprocess.Popen([str(busy_bin), str(args.hogs), "userInitiated"], stdout=log, stderr=log)
                            time.sleep(2)
                            if hog.poll() is not None:
                                raise RuntimeError("CPU load process exited")
                        proc.stdin.write(b"go\n")
                        proc.stdin.close()
                    elif record["event"] == "sample":
                        measured += 1
            code = proc.wait(timeout=5)
            if code != 0 or measured != args.runs * len(clips):
                raise RuntimeError(f"exit {code}, {measured} samples")
            print(f"round {round_id} {condition} {mode}: {measured} samples", flush=True)
    except (RuntimeError, TimeoutError, subprocess.TimeoutExpired) as error:
        output.write(json.dumps(dict(event="error", mode=mode, condition=condition, round=round_id, error=str(error))) + "\n")
        output.flush()
        print(f"round {round_id} {condition} {mode}: {error}", flush=True)
    finally:
        stop(hog)
        stop(proc)
    time.sleep(2)


with (args.out / "raw.jsonl").open("w") as output:
    for round_id in range(args.rounds):
        modes = args.modes if round_id % 2 == 0 else list(reversed(args.modes))
        conditions = ["normal", "cpu-load"] if round_id % 2 == 0 else ["cpu-load", "normal"]
        for condition in conditions:
            for mode in modes:
                case(mode, condition, round_id, output)
