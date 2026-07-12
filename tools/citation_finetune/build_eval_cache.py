#!/usr/bin/env python3
"""Consolidate the full labelled cite set and warm the persistent decode cache.

Produces two durable artifacts under datasets/citation_finetune/:
  - eval_labelled_all.jsonl : {id, input, target} for every distinct labelled cite
    (union of the heldout gold + the training split, heldout gold preferred).
  - decode_cache.jsonl      : {input, raw, orig_issue, praw, praw_issue, has_perturb}
    keyed by normalized input, warm-started from every scratchpad_raws*.jsonl we already
    decoded so the model is never re-run on a cite we've already seen.

`id` is a stable 16-hex sha1 of the normalized input, so gold, cache, and decodes all
join on the same key regardless of which file a cite originally came from.
"""
import glob
import hashlib
import json
import os

ROOT = "/home/adam/Projects/flowstate"
OUT_GOLD = f"{ROOT}/datasets/citation_finetune/eval_labelled_all.jsonl"
OUT_CACHE = f"{ROOT}/datasets/citation_finetune/decode_cache.jsonl"


def norm(s: str) -> str:
    return s.replace("parse citation:", "").strip()


def cid(inp: str) -> str:
    return hashlib.sha1(norm(inp).encode()).hexdigest()[:16]


def main() -> None:
    # 1. Gold: heldout preferred (curated/Sonnet), else training split.
    gold: dict[str, dict] = {}
    # lower priority first so heldout overwrites train
    sources = [
        ("datasets/citation_finetune/train_full_clean_dedup.jsonl", "input", "target"),
        ("scratchpad_heldout.jsonl", "input", "target"),
    ]
    for rel, ik, tk in sources:
        path = f"{ROOT}/{rel}"
        if not os.path.exists(path):
            continue
        for line in open(path):
            if not line.strip():
                continue
            v = json.loads(line)
            inp = v.get(ik, "")
            if not norm(inp):
                continue
            full_input = inp if inp.strip().startswith("parse citation:") else f"parse citation: {norm(inp)}"
            gold[cid(inp)] = {"id": cid(inp), "input": full_input, "target": v.get(tk)}

    with open(OUT_GOLD, "w") as f:
        for r in gold.values():
            f.write(json.dumps(r) + "\n")
    print(f"gold: {len(gold)} distinct labelled cites -> {OUT_GOLD}")

    # 2. Warm the decode cache from every raws file we've already produced.
    cache: dict[str, dict] = {}
    for path in glob.glob(f"{ROOT}/scratchpad_raws*.jsonl"):
        for line in open(path):
            if not line.strip():
                continue
            r = json.loads(line)
            inp = r.get("input", "")
            if "raw" not in r or not norm(inp):
                continue
            key = norm(inp)
            # prefer a record that carries a perturbation (fuller) if we've seen this cite twice
            if key in cache and cache[key].get("has_perturb") and not r.get("has_perturb"):
                continue
            cache[key] = {
                "input": inp,
                "raw": r["raw"],
                "orig_issue": r.get("orig_issue"),
                "praw": r.get("praw", ""),
                "praw_issue": r.get("praw_issue"),
                "has_perturb": r.get("has_perturb", False),
            }
    with open(OUT_CACHE, "w") as f:
        for r in cache.values():
            f.write(json.dumps(r) + "\n")
    print(f"cache warmed: {len(cache)} decoded cites -> {OUT_CACHE}")

    # 3. Report the decode gap over the labelled set.
    need = [g["input"] for g in gold.values() if norm(g["input"]) not in cache]
    print(f"labelled cites still needing decode: {len(need)} / {len(gold)}")
    with open(f"{ROOT}/datasets/citation_finetune/decode_todo.jsonl", "w") as f:
        for inp in need:
            f.write(json.dumps({"id": cid(inp), "input": inp}) + "\n")
    print("wrote decode_todo.jsonl")


if __name__ == "__main__":
    main()
