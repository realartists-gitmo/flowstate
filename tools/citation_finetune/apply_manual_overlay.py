#!/usr/bin/env python3
"""Apply manual label corrections to draft labels and rebuild review/train files."""

from __future__ import annotations

import argparse
import json
from pathlib import Path

import draft_label


DEFAULT_DRAFT = Path("datasets/citation_finetune/labels_full_draft.jsonl")
DEFAULT_OVERLAY = Path("datasets/citation_finetune/manual_review_overlay.jsonl")
DEFAULT_REVIEW = Path("datasets/citation_finetune/labels_full_review_queue.jsonl")
DEFAULT_SUMMARY = Path("datasets/citation_finetune/labels_full_summary.json")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Apply manual review overlay.")
    parser.add_argument("--draft", type=Path, default=DEFAULT_DRAFT)
    parser.add_argument("--overlay", type=Path, default=DEFAULT_OVERLAY)
    parser.add_argument("--review", type=Path, default=DEFAULT_REVIEW)
    parser.add_argument("--summary", type=Path, default=DEFAULT_SUMMARY)
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    overlays = {}
    if args.overlay.exists():
        with args.overlay.open(encoding="utf-8") as src:
            for line in src:
                row = json.loads(line)
                overlays[row["id"]] = row["label_json"]

    rows = []
    with args.draft.open(encoding="utf-8") as src:
        for line in src:
            row = json.loads(line)
            if row["id"] in overlays:
                row["label_json"] = overlays[row["id"]]
            draft_label.validate_label(row["label_json"])
            rows.append(row)

    with args.draft.open("w", encoding="utf-8") as dst, args.review.open(
        "w", encoding="utf-8"
    ) as review:
        by_stratum = {}
        by_status = {}
        by_warning = {}
        review_count = 0
        for row in rows:
            dst.write(json.dumps(row, ensure_ascii=False, separators=(",", ":")) + "\n")
            label = row["label_json"]
            by_stratum[row["primary_stratum"]] = by_stratum.get(row["primary_stratum"], 0) + 1
            by_status[label["status"]] = by_status.get(label["status"], 0) + 1
            for warning in label.get("warnings", []):
                by_warning[warning] = by_warning.get(warning, 0) + 1
            if draft_label.needs_review(row, label) and row["id"] not in overlays:
                review.write(json.dumps(row, ensure_ascii=False, separators=(",", ":")) + "\n")
                review_count += 1

    summary = {
        "input": str(args.draft),
        "manual_overlay": str(args.overlay),
        "manual_overlay_count": len(overlays),
        "output": str(args.draft),
        "review": str(args.review),
        "row_count": len(rows),
        "review_count": review_count,
        "by_stratum": by_stratum,
        "by_status": by_status,
        "by_warning": by_warning,
    }
    args.summary.write_text(json.dumps(summary, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(json.dumps(summary, indent=2, ensure_ascii=False))


if __name__ == "__main__":
    main()
