#!/usr/bin/env python3
"""Deterministic per-stratum train/eval/test split of the training JSONL.

Splitting is stratified WITHIN each primary_stratum (never a global random
split) so every stratum — including the reject class — is represented in all
three partitions.  Assignment is a stable hash of the row id, so re-runs are
reproducible and adding rows does not reshuffle existing ones.  Targets are
copied verbatim.

The natural-distribution gold set (gold_labeled.jsonl) is a SEPARATE held-out
test set and is intentionally not mixed in here.
"""

from __future__ import annotations

import argparse
import hashlib
import json
from collections import Counter, defaultdict
from pathlib import Path

DEFAULT_INPUT = Path("datasets/citation_finetune/train_full_draft.jsonl")
DEFAULT_OUT_DIR = Path("datasets/citation_finetune/splits")


def parse_args() -> argparse.Namespace:
    ap = argparse.ArgumentParser(description="Stratified train/eval/test split.")
    ap.add_argument("--input", type=Path, default=DEFAULT_INPUT)
    ap.add_argument("--out-dir", type=Path, default=DEFAULT_OUT_DIR)
    ap.add_argument("--eval-frac", type=float, default=0.10)
    ap.add_argument("--test-frac", type=float, default=0.10)
    ap.add_argument("--salt", default="flowstate-cite-v1")
    return ap.parse_args()


def bucket(row_id: str, salt: str) -> float:
    h = hashlib.sha256(f"{salt}:{row_id}".encode()).hexdigest()
    return int(h[:8], 16) / 0xFFFFFFFF


def main() -> None:
    args = parse_args()
    rows = [json.loads(l) for l in args.input.open(encoding="utf-8")]
    args.out_dir.mkdir(parents=True, exist_ok=True)

    eval_hi = args.eval_frac
    test_hi = args.eval_frac + args.test_frac

    by_split: dict[str, list] = {"train": [], "eval": [], "test": []}
    # Assign within each stratum so ratios hold per-stratum, not just globally.
    per_stratum: dict[str, list] = defaultdict(list)
    for r in rows:
        per_stratum[r.get("primary_stratum", "unknown")].append(r)

    for stratum, srows in per_stratum.items():
        srows.sort(key=lambda r: bucket(r["id"], args.salt))
        n = len(srows)
        e = round(n * args.eval_frac)
        t = round(n * args.test_frac)
        for idx, r in enumerate(srows):
            if idx < e:
                by_split["eval"].append(r)
            elif idx < e + t:
                by_split["test"].append(r)
            else:
                by_split["train"].append(r)

    counts = {}
    for split, srows in by_split.items():
        path = args.out_dir / f"{split}.jsonl"
        with path.open("w", encoding="utf-8") as f:
            for r in srows:
                f.write(json.dumps({"id": r["id"], "input": r["input"], "target": r["target"]}, ensure_ascii=False) + "\n")
        counts[split] = len(srows)

    manifest = {
        "input": str(args.input),
        "eval_frac": args.eval_frac,
        "test_frac": args.test_frac,
        "salt": args.salt,
        "total": len(rows),
        "counts": counts,
        "per_stratum": {
            s: {
                "train": sum(1 for r in by_split["train"] if r.get("primary_stratum") == s),
                "eval": sum(1 for r in by_split["eval"] if r.get("primary_stratum") == s),
                "test": sum(1 for r in by_split["test"] if r.get("primary_stratum") == s),
            }
            for s in sorted(per_stratum)
        },
        "note": "gold_labeled.jsonl is a separate natural-distribution held-out test set.",
    }
    (args.out_dir / "split_manifest.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"counts": counts, "strata": len(per_stratum)}, indent=2))


if __name__ == "__main__":
    main()
