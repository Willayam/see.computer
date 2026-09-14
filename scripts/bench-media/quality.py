"""Extract comparable frames and score OCR drift against the uncompressed fixture."""
import argparse
import json
from pathlib import Path
import re
import subprocess


def command(*args):
    subprocess.run(args, check=True, stdout=subprocess.DEVNULL)


def distance(a, b):
    previous = list(range(len(b) + 1))
    for i, left in enumerate(a, 1):
        current = [i]
        for j, right in enumerate(b, 1):
            current.append(min(current[-1]+1, previous[j]+1, previous[j-1]+(left != right)))
        previous = current
    return previous[-1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(".audit/media-bench"))
    args = parser.parse_args()
    out = args.root / "quality"
    out.mkdir(exist_ok=True)
    fixture = str(args.root / "fixture")
    records = []
    references = {}
    for second in [2, 6, 10]:
        path = out / f"reference-{second}.png"
        command("ffmpeg", "-y", "-hide_banner", "-loglevel", "error", "-ss", str(second), "-i", str(args.root/"source.mkv"), "-frames:v", "1", str(path))
        references[second] = json.loads(subprocess.check_output([fixture, "ocr", str(path)]))
    for profile in ["1440p-15-2m", "1440p-15-4m", "1440p-30-4m", "1080p-15-2m"]:
        playlist = args.root / "matrix" / f"0-normal-{profile}" / "index.m3u8"
        movie = out / f"{profile}.mp4"
        command("ffmpeg", "-y", "-hide_banner", "-loglevel", "error", "-i", str(playlist), "-c", "copy", "-movflags", "+faststart", str(movie))
        for second in [2, 6, 10]:
            path = out / f"{profile}-{second}.png"
            command("ffmpeg", "-y", "-hide_banner", "-loglevel", "error", "-ss", str(second), "-i", str(movie), "-frames:v", "1", str(path))
            actual = json.loads(subprocess.check_output([fixture, "ocr", str(path)]))
            # Recognition drift includes the OCR engine's own errors; it is not human transcription accuracy.
            expected_words = re.findall(r"\w+", " ".join(references[second]).casefold())
            actual_words = re.findall(r"\w+", " ".join(actual).casefold())
            records.append({"profile": profile, "second": second, "reference_ocr": references[second],
                "output_ocr": actual, "word_edits": distance(expected_words, actual_words), "reference_words": len(expected_words)})
    reference = out / "reference-2.png"
    jpeg = out / "screenshot-q4.jpg"
    command("ffmpeg", "-y", "-hide_banner", "-loglevel", "error", "-i", str(reference), "-frames:v", "1", "-q:v", "4", str(jpeg))
    image_ocr = json.loads(subprocess.check_output([fixture, "ocr", str(jpeg)]))
    (out/"quality.json").write_text(json.dumps({"frames": records,
        "screenshot": {"png_bytes": reference.stat().st_size, "jpeg_q4_bytes": jpeg.stat().st_size,
            "png_ocr": references[2], "jpeg_ocr": image_ocr}}, ensure_ascii=False, indent=2))
    for profile in sorted({r['profile'] for r in records}):
        group = [r for r in records if r['profile'] == profile]
        print(profile, 'OCR word edits:', sum(r['word_edits'] for r in group), '/', sum(r['reference_words'] for r in group))


if __name__ == "__main__":
    main()
