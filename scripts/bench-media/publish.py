"""Upload only synthetic benchmark assets to the isolated R2 Worker and verify reads."""
import argparse
import hashlib
import html
import http.client
import json
from pathlib import Path
import secrets
import shutil
import subprocess
import time
from urllib.parse import urlsplit


def upload_once(endpoint, token, run_id, path, name=None, connection=None):
    target = urlsplit(endpoint)
    if target.scheme != "https" or target.path not in ["", "/"]:
        raise ValueError("Use the HTTPS Worker origin")
    route = f"/d/{run_id}/{name or path.name}"
    began = time.monotonic()
    connection = connection or http.client.HTTPSConnection(target.netloc, timeout=30)
    connection.putrequest("PUT", route)
    connection.putheader("Authorization", "Bearer " + token)
    connection.putheader("Content-Length", str(path.stat().st_size))
    connection.endheaders()
    with path.open("rb") as source:
        while chunk := source.read(65536):
            connection.send(chunk)
    response = connection.getresponse()
    response.read()
    status = response.status
    if status != 201:
        raise RuntimeError(f"Upload returned HTTP {status}")
    elapsed = (time.monotonic()-began)*1000
    connection.request("GET", route)
    response = connection.getresponse()
    digest = hashlib.sha256()
    while chunk := response.read(65536):
        digest.update(chunk)
    assert response.status == 200 and digest.hexdigest() == hashlib.sha256(path.read_bytes()).hexdigest()
    return {"file": name or path.name, "bytes": path.stat().st_size, "upload_ms": elapsed,
            "sha256": digest.hexdigest(), "url": endpoint.rstrip("/") + route}


def upload(endpoint, token, run_id, path, name=None, connection=None):
    attempts = []
    started = time.monotonic()
    for attempt in range(5):
        began = time.monotonic()
        try:
            result = upload_once(endpoint, token, run_id, path, name, connection)
            attempts.append({"attempt": attempt, "success": True, "elapsed_ms": (time.monotonic()-began)*1000})
            return result | {"attempts": attempts, "elapsed_with_retries_ms": (time.monotonic()-started)*1000}
        except (OSError, http.client.HTTPException) as error:
            if connection:
                connection.close()
            attempts.append({"attempt": attempt, "error": str(error), "elapsed_ms": (time.monotonic()-began)*1000})
            if attempt == 4:
                raise RuntimeError(json.dumps({"file": path.name, "attempts": attempts})) from error
            time.sleep(.25 * 2**attempt)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--endpoint", required=True)
    parser.add_argument("--token-file", type=Path, required=True)
    parser.add_argument("--root", type=Path, default=Path(".audit/media-bench"))
    parser.add_argument("--live", action="store_true")
    parser.add_argument("--out", type=Path)
    args = parser.parse_args()
    token = args.token_file.read_text().strip()
    run_id = secrets.token_hex(16)
    out = args.out or args.root / ("r2-live" if args.live else "r2-published")
    out.mkdir(exist_ok=False)
    events = []
    connection = http.client.HTTPSConnection(urlsplit(args.endpoint).netloc, timeout=30)
    if args.live:
        shutil.copy2(args.root/"page.png", out/"page.png")
        directory = out/"segments"
        directory.mkdir()
        began = time.monotonic()
        with (out/"capture.json").open("w") as log, (out/"capture.log").open("w") as errors:
            recorder = subprocess.Popen([str(args.root/"fixture"), "capture-segments", str(directory), "12"], stdout=log, stderr=errors)
            seen = set()
            try:
                while time.monotonic()-began < 90:
                    paths = sorted([*directory.glob("*.m4s"), *directory.glob("init.mp4")], key=lambda p: (p.name != "init.mp4", p.name))
                    for path in paths:
                        if path.name in seen:
                            continue
                        record = upload(args.endpoint, token, run_id, path, connection=connection)
                        record["verified_ms_from_start"] = (time.monotonic()-began)*1000
                        record["recording_process_running"] = recorder.poll() is None
                        events.append(record)
                        seen.add(path.name)
                    # More segments can appear during a slow upload. Rescan after the writer exits.
                    expected = {p.name for p in [*directory.glob("*.m4s"), *directory.glob("init.mp4")]}
                    if recorder.poll() is not None and expected <= seen:
                        if recorder.returncode:
                            raise RuntimeError("Native capture failed")
                        break
                    time.sleep(.02)
                else:
                    raise TimeoutError("Live upload test exceeded 90 seconds")
            finally:
                if recorder.poll() is None:
                    recorder.terminate()
                    recorder.wait(timeout=10)
        complete = out/"native-capture.mp4"
        with complete.open("wb") as movie:
            for path in [directory/"init.mp4", *sorted(directory.glob("*.m4s"))]:
                movie.write(path.read_bytes())
        expected = {event["file"] for event in json.loads((out/"capture.json").read_text())["segments"]}
        assert expected == seen, "Every emitted segment must have a verified remote copy"
        events.append(upload(args.endpoint, token, run_id, complete, connection=connection))
    else:
        paths = sorted((args.root/"quality").glob("*.mp4")) + [args.root/"quality/reference-2.png"]
        for path in paths:
            events.append(upload(args.endpoint, token, run_id, path, connection=connection))
    videos = [e for e in events if e["file"].endswith(".mp4") and e["file"] != "init.mp4"]
    page = out/"index.html"
    figures = "\n".join(f'<figure><figcaption>{html.escape(e["file"])} · {e["bytes"]/1_000_000:.2f} MB</figcaption>'
        f'<video controls muted playsinline preload="none" src="{html.escape(e["url"], quote=True)}"></video>'
        f'<p><a href="{html.escape(e["url"], quote=True)}" download>Download video</a></p></figure>' for e in videos)
    page.write_text('<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width">'
        '<title>see.computer recording test</title><style>body{max-width:960px;margin:64px auto;padding:0 24px;background:#f7f8fa;'
        'color:#202327;font:18px/1.6 system-ui}h1{font-size:clamp(32px,5vw,52px);line-height:1.1;letter-spacing:-.04em}'
        'figure{margin:48px 0;border-top:1px solid #c9ced4;padding-top:20px}figcaption{margin-bottom:12px;font-weight:600}'
        'video{width:100%;aspect-ratio:16/9;background:#e5e8ed}a{color:#164fba}a:focus-visible{outline:3px solid #164fba;outline-offset:4px}'
        '</style><main><a href="https://see.computer">see.computer</a><h1>Recording compression test</h1>'
        '<p>A 12-second synthetic screen recording, compressed locally with hardware H.264 encoding and hosted on Cloudflare R2. '
        'Compare the code and Swedish text while the page scrolls.</p><p>This is a recording experiment. '
        'Sharing is not yet integrated into the desktop app.</p>' + figures +
        '<p><a href="take.md">Recording index for agents</a></p></main></html>')
    events.append(upload(args.endpoint, token, run_id, page, connection=connection))
    index = out/"take.md"
    index.write_text("# see.computer compression test\n\nSynthetic screen content only. No personal recordings.\n\n" +
        "\n".join(f"- [{e['file']}]({e['url']})" for e in events) + "\n")
    events.append(upload(args.endpoint, token, run_id, index, connection=connection))
    (out/"uploads.json").write_text(json.dumps({"run_id": run_id, "endpoint": args.endpoint, "live": args.live, "uploads": events}, indent=2))
    print(next(e["url"] for e in events if e["file"] == "index.html"))
    connection.close()


if __name__ == "__main__":
    main()
