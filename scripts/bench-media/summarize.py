"""Summarize the controlled encoding matrix without pooling separate network experiments."""
import argparse
import csv
import json
import math
from pathlib import Path
import statistics


def summary(rows):
    result = []
    for profile in sorted({r["profile"] for r in rows}):
        for condition in sorted({r["condition"] for r in rows}):
            group = [r for r in rows if r["profile"] == profile and r["condition"] == condition]
            if not group:
                continue
            successful = [r for r in group if "error" not in r]
            samples = sorted(s["ms"] for r in successful for s in r["stt_samples"])
            row = {"profile": profile, "condition": condition, "attempts": len(group), "completed": len(successful),
                "stt_samples": len(samples), "stt_median_ms": round(statistics.median(samples), 1) if samples else None,
                "stt_p95_ms": round(samples[math.ceil(len(samples)*.95)-1], 1) if samples else None}
            for metric in ["bytes", "first_media_upload_ms", "upload_tail_ms", "max_queued_bytes",
                           "maximum_resident_set_size", "peak_memory_footprint", "encoder_cpu_user_seconds", "encoder_cpu_system_seconds"]:
                values = [r[metric] for r in successful if r.get(metric) is not None]
                row[metric + "_mean"] = round(statistics.mean(values), 2) if values else None
            result.append(row)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    args = parser.parse_args()
    rows = [json.loads(line) for line in (args.directory/"raw.jsonl").read_text().splitlines()]
    result = summary(rows)
    with (args.directory/"summary.csv").open("w", newline="") as output:
        writer = csv.DictWriter(output, fieldnames=result[0].keys())
        writer.writeheader()
        writer.writerows(result)
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
