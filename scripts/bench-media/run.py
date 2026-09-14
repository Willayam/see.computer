"""Paced hardware encoding, local STT contention, and bounded upload simulation."""
import argparse
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import http.client
import json
import os
from pathlib import Path
import random
import re
import selectors
import subprocess
import threading
import time

ROOT = Path(__file__).resolve().parents[2]
PROFILES = {
    "1440p-15-2m": (2560, 1440, 15, "2M"),
    "1440p-15-4m": (2560, 1440, 15, "4M"),
    "1440p-30-4m": (2560, 1440, 30, "4M"),
    "1080p-15-2m": (1920, 1080, 15, "2M"),
}


def stop(process):
    if process is not None and process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()


class Receiver(BaseHTTPRequestHandler):
    def do_PUT(self):
        name = Path(self.path).name
        remaining = int(self.headers["Content-Length"])
        digest = hashlib.sha256()
        with (self.server.destination / name).open("wb") as output:
            while remaining:
                chunk = self.rfile.read(min(65536, remaining))
                if not chunk:
                    break
                output.write(chunk)
                digest.update(chunk)
                remaining -= len(chunk)
        if self.server.fail_once and not self.server.failed:
            self.server.failed = True
            self.send_response(503)
        else:
            self.server.hashes[name] = digest.hexdigest()
            self.send_response(201 if remaining == 0 else 400)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def log_message(self, *args):
        pass


class Upload:
    def __init__(self, directory, mbps, fail_once=False):
        self.directory = directory
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Receiver)
        self.server.destination = directory / "received"
        self.server.destination.mkdir()
        self.server.hashes = {}
        self.server.fail_once = fail_once
        self.server.failed = False
        self.server.daemon_threads = True
        self.mbps = mbps
        self.events = []
        self.finished = threading.Event()
        self.cancel = threading.Event()
        self.seen = set()
        self.max_queued = 0
        self.error = None
        self.origin = time.monotonic()
        self.serving = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.worker = threading.Thread(target=self.run, daemon=True)

    def start(self):
        self.origin = time.monotonic()
        self.serving.start()
        self.worker.start()

    def pending(self):
        paths = sorted([*self.directory.glob("*.m4s"), *self.directory.glob("init.mp4")], key=lambda p: (p.name != "init.mp4", p.name))
        result = [p for p in paths if p.name not in self.seen]
        self.max_queued = max(self.max_queued, sum(p.stat().st_size for p in result))
        return result

    def run(self):
        try:
            while not self.cancel.is_set():
                pending = self.pending()
                if not pending:
                    if self.finished.is_set():
                        break
                    time.sleep(.02)
                    continue
                path = pending[0]
                # init.mp4 has no atomic-rename guarantee. Wait for the first finalized media segment.
                if path.name == "init.mp4" and not list(self.directory.glob("*.m4s")):
                    time.sleep(.02)
                    continue
                began = time.monotonic()
                connection = http.client.HTTPConnection(*self.server.server_address, timeout=15)
                connection.putrequest("PUT", "/" + path.name)
                connection.putheader("Content-Length", str(path.stat().st_size))
                connection.endheaders()
                with path.open("rb") as source:
                    while chunk := source.read(65536):
                        if self.cancel.is_set():
                            raise RuntimeError("upload cancelled")
                        time.sleep(len(chunk) * 8 / (self.mbps * 1_000_000))
                        connection.send(chunk)
                        self.pending()
                time.sleep(.05)  # Simulated 50 ms request/response overhead.
                response = connection.getresponse()
                response.read()
                status = response.status
                connection.close()
                self.events.append({"file": path.name, "bytes": path.stat().st_size,
                    "start_ms": (began-self.origin)*1000, "done_ms": (time.monotonic()-self.origin)*1000, "status": status})
                if status == 201:
                    assert hashlib.sha256(path.read_bytes()).hexdigest() == self.server.hashes[path.name]
                    self.seen.add(path.name)
                else:
                    time.sleep(.25)
        except Exception as error:
            self.error = str(error)

    def close(self):
        self.finished.set()
        self.worker.join(timeout=150)
        if self.worker.is_alive():
            self.cancel.set()
            self.worker.join(timeout=20)
            self.error = "upload timeout"
        self.server.shutdown()
        self.server.server_close()
        if self.error:
            raise RuntimeError(self.error)


def start_stt(directory):
    model = Path.home() / "Library/Application Support/see.computer/models/parakeet-tdt-0.6b-v3-onnx/int8-prepared"
    command = [str(ROOT / "scripts/bench-backends/target/release/see-backend-bench"), "cpu8", str(model), "8",
               str(directory / "model-cache"), str(ROOT / "fixtures/en.wav"), str(ROOT / "fixtures/sv.wav")]
    log = (directory / "stt.log").open("w")
    process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log, bufsize=0)
    with selectors.DefaultSelector() as selector:
        selector.register(process.stdout, selectors.EVENT_READ)
        pending = b""
        while True:
            if not selector.select(timeout=60):
                stop(process)
                raise TimeoutError("STT warmup timeout")
            chunk = os.read(process.stdout.fileno(), 65536)
            if not chunk:
                raise RuntimeError("STT exited before ready")
            pending += chunk
            while b"\n" in pending:
                line, pending = pending.split(b"\n", 1)
                if json.loads(line)["event"] == "ready":
                    return process, log
    return process, log


def encoder_command(source, profile, directory):
    width, height, fps, bitrate = PROFILES[profile]
    return ["/usr/bin/time", "-l", "ffmpeg", "-y", "-hide_banner", "-loglevel", "warning", "-threads", "2", "-re", "-i", str(source),
            "-an", "-vf", f"fps={fps},scale={width}:{height}:flags=lanczos", "-filter_threads", "1",
            "-c:v", "h264_videotoolbox", "-allow_sw", "0", "-realtime", "1", "-b:v", bitrate,
            "-g", str(fps), "-bf", "0", "-pix_fmt", "yuv420p", "-f", "hls", "-hls_time", "1",
            "-hls_list_size", "0", "-hls_segment_type", "fmp4", "-hls_flags", "independent_segments+temp_file",
            "-hls_segment_filename", str(directory / "segment-%03d.m4s"), str(directory / "index.m3u8")]


def trial(args, profile, condition, round_id):
    directory = args.out / f"{round_id}-{condition}-{profile}"
    directory.mkdir()
    stt = encoder = hog = None
    upload = None
    record = {"profile": profile, "condition": condition, "round": round_id}
    began = time.monotonic()
    try:
        stt, stt_log = start_stt(directory)
        if condition == "cpu-load":
            hog = subprocess.Popen([str(args.busy), "14", "userInitiated"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            time.sleep(1)
        if profile != "baseline":
            upload = Upload(directory, .5 if condition == "slow-network" else 5, condition == "slow-network")
            upload.start()
            began = time.monotonic()
            command = encoder_command(args.source, profile, directory)
            record["command"] = command
            encode_log = (directory / "encode.log").open("w")
            encoder = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=encode_log)
        time.sleep(2)
        stt_began = time.monotonic()
        stdout, _ = stt.communicate(b"go\n", timeout=60)
        stt_log.close()
        if stt.returncode:
            raise RuntimeError("STT failed")
        samples = [json.loads(line) for line in stdout.splitlines() if json.loads(line)["event"] == "sample"]
        if len(samples) != 16:
            raise RuntimeError(f"Expected 16 transcriptions, got {len(samples)}")
        record["stt_samples"] = samples
        record["stt_start_ms"] = (stt_began-began)*1000
        record["stt_finish_ms"] = (time.monotonic()-began)*1000
        if encoder:
            if encoder.wait(timeout=60):
                raise RuntimeError("Hardware encoding failed; inspect encode.log")
            finished = time.monotonic()
            encode_log.close()
            record["encoding_finished_ms"] = (finished-began)*1000
            upload.close()
            record["upload_events"] = upload.events
            record["max_queued_bytes"] = upload.max_queued
            record["upload_read_buffer_bytes"] = 65536
            record["network"] = {"kind": "loopback simulation", "mbps": upload.mbps, "request_overhead_ms": 50}
            record["last_upload_ms"] = max(e["done_ms"] for e in upload.events)
            record["upload_tail_ms"] = max(0, record["last_upload_ms"]-record["encoding_finished_ms"])
            record["first_media_upload_ms"] = next(e["done_ms"] for e in upload.events if e["file"].endswith(".m4s") and e["status"] == 201)
            record["bytes"] = sum(p.stat().st_size for p in directory.glob("*.m4s")) + (directory/"init.mp4").stat().st_size
            log = (directory / "encode.log").read_text()
            for label in ["maximum resident set size", "peak memory footprint"]:
                match = re.search(r"(\d+)\s+" + label, log)
                record[label.replace(" ", "_")] = int(match[1]) if match else None
            timing = re.search(r"([\d.]+) real\s+([\d.]+) user\s+([\d.]+) sys", log)
            if timing:
                record["encoder_wall_seconds"], record["encoder_cpu_user_seconds"], record["encoder_cpu_system_seconds"] = map(float, timing.groups())
        print(f"{round_id} {condition} {profile}: complete", flush=True)
    except Exception as error:
        record["error"] = str(error)
        print(f"{round_id} {condition} {profile}: {error}", flush=True)
    finally:
        stop(stt); stop(encoder); stop(hog)
        if upload and upload.serving.is_alive():
            upload.cancel.set()
            upload.close()
    return record


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, default=ROOT / ".audit/media-bench/source.mkv")
    parser.add_argument("--out", type=Path, default=ROOT / ".audit/media-bench/results")
    parser.add_argument("--busy", type=Path, default=ROOT / ".audit/backend-bench/results/busy")
    parser.add_argument("--rounds", type=int, default=2)
    parser.add_argument("--slow-only", action="store_true")
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=False)
    (args.out / "metadata.json").write_text(json.dumps({"started": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "source_sha256": hashlib.sha256(args.source.read_bytes()).hexdigest(), "rounds": args.rounds,
        "profiles": PROFILES, "ffmpeg": subprocess.check_output(["ffmpeg", "-version"], text=True).splitlines()[0],
        "network": "Loopback HTTP with controlled sender rate and 50 ms overhead. Not R2 latency."}, indent=2))
    with (args.out / "raw.jsonl").open("w") as output:
        for round_id in range(1 if args.slow_only else args.rounds):
            order = [("1440p-15-2m", "slow-network")] if args.slow_only else [(p, c) for p in ["baseline", *PROFILES] for c in ["normal", "cpu-load"]]
            random.Random(731+round_id).shuffle(order)
            for profile, condition in order:
                output.write(json.dumps(trial(args, profile, condition, round_id)) + "\n")
                output.flush()


if __name__ == "__main__":
    main()
