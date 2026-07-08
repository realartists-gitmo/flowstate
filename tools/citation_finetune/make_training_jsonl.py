#!/usr/bin/env python3
"""Convert draft labels into compact seq2seq fine-tune JSONL.

The training *target* is the sparse projection produced by
``draft_label.to_target`` — populated fields only, canonical key order, no
labeler bookkeeping (confidence / inferred_fields).  This roughly halves target
length versus the old dense 26-key-always schema and concentrates the learning
signal on fields that actually carry information.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

import draft_label


DEFAULT_INPUT = Path("datasets/citation_finetune/labels_full_draft.jsonl")
DEFAULT_OUTPUT = Path("datasets/citation_finetune/train_full_draft.jsonl")
DEFAULT_LLM_RELABEL = Path("datasets/citation_finetune/llm_relabel.jsonl")
DEFAULT_OVERLAY = Path("datasets/citation_finetune/manual_review_overlay.jsonl")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Build seq2seq training JSONL.")
    parser.add_argument("--input", type=Path, default=DEFAULT_INPUT)
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument("--llm-relabel", type=Path, default=DEFAULT_LLM_RELABEL)
    parser.add_argument("--overlay", type=Path, default=DEFAULT_OVERLAY)
    return parser.parse_args()


def _load_ids(path: Path, key: str = "id") -> set:
    if not path.exists():
        return set()
    return {json.loads(l)[key] for l in path.open(encoding="utf-8")}


def _load_relabel(path: Path) -> dict:
    if not path.exists():
        return {}
    out = {}
    for line in path.open(encoding="utf-8"):
        r = json.loads(line)
        out[r["id"]] = r["target"]
    return out


def main() -> None:
    args = parse_args()
    # Precedence for the training target: manual overlay > LLM relabel > heuristic.
    manual_ids = _load_ids(args.overlay)
    llm = _load_relabel(args.llm_relabel)
    src_counts = {"manual": 0, "llm": 0, "heuristic": 0}
    n = 0
    with args.input.open(encoding="utf-8") as src, args.output.open("w", encoding="utf-8") as dst:
        for line in src:
            row = json.loads(line)
            rid = row["id"]
            label = row["label_json"]
            draft_label.validate_label(label)
            if rid in manual_ids:
                target_obj = draft_label.to_target(label)
                src_counts["manual"] += 1
            elif rid in llm:
                target_obj = draft_label.normalize_target(llm[rid])
                src_counts["llm"] += 1
            else:
                target_obj = draft_label.to_target(label)
                src_counts["heuristic"] += 1
            record = {
                "id": rid,
                "primary_stratum": row["primary_stratum"],
                "input": "parse citation: " + row["fullcite"],
                "target": json.dumps(target_obj, ensure_ascii=False, separators=(",", ":")),
            }
            dst.write(json.dumps(record, ensure_ascii=False, separators=(",", ":")) + "\n")
            n += 1
    print(f"Wrote {n} rows to {args.output}  sources={src_counts}")


if __name__ == "__main__":
    main()
