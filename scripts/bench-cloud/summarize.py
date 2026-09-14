"""Summarize successful timings and retain failed attempts separately."""
import argparse
from collections import Counter
import csv
import json
import math
from pathlib import Path
import statistics


def percentile(values, fraction):
    return sorted(values)[max(0, math.ceil(len(values) * fraction) - 1)] if values else None


def summarize(rows):
    successful = [row for row in rows if "error" not in row]
    result = {"attempts": len(rows), "completed": len(successful), "failed": len(rows) - len(successful)}
    for metric in ["stop_to_final_ms", "commit_to_final_ms", "setup_ms", "first_text_ms", "first_audio_ms"]:
        values = [row[metric] for row in successful if row.get(metric) is not None]
        result[metric + "_median"] = round(statistics.median(values), 1) if values else None
        p95 = percentile(values, .95)
        result[metric + "_p95"] = round(p95, 1) if p95 is not None else None
    result["word_errors"] = sum(row["word_errors"] for row in successful)
    result["reference_words"] = sum(row["reference_words"] for row in successful)
    result["wer_percent"] = round(100 * result["word_errors"] / result["reference_words"], 2) if result["reference_words"] else None
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    args = parser.parse_args()
    rows = [json.loads(line) for line in (args.directory / "raw.jsonl").read_text().splitlines()]
    summaries = []
    transcripts = []
    for provider in sorted({row["provider"] for row in rows}):
        group = [row for row in rows if row["provider"] == provider]
        for clip in ["all", *sorted({row["clip"] for row in group})]:
            subset = group if clip == "all" else [row for row in group if row["clip"] == clip]
            summaries.append({"provider": provider, "clip": clip, **summarize(subset)})
            if clip != "all":
                transcripts.append({"provider": provider, "clip": clip,
                                    "transcripts": dict(Counter(row["text"] for row in subset if "error" not in row)),
                                    "errors": dict(Counter(row["error"] for row in subset if "error" in row))})
    with (args.directory / "summary.csv").open("w", newline="") as output:
        writer = csv.DictWriter(output, fieldnames=list(summaries[0]))
        writer.writeheader()
        writer.writerows(summaries)
    (args.directory / "transcripts.json").write_text(json.dumps(transcripts, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps([row for row in summaries if row["clip"] == "all"], indent=2))


if __name__ == "__main__":
    main()
