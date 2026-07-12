#!/usr/bin/env python3
"""Join the persistent decode cache against the labelled gold set (by normalized input) and emit
replay-ready raws keyed by the stable cid, so `replay labelled_raws.jsonl eval_labelled_all.jsonl`
scores every decoded labelled cite. Reports decode coverage over the labelled set.
"""
import hashlib
import json

ROOT = "/home/adam/Projects/flowstate"
GOLD = f"{ROOT}/datasets/citation_finetune/eval_labelled_all.jsonl"
CACHE = f"{ROOT}/datasets/citation_finetune/decode_cache.jsonl"
OUT = f"{ROOT}/datasets/citation_finetune/labelled_raws.jsonl"


def norm(s: str) -> str:
    return s.replace("parse citation:", "").strip()


def cid(inp: str) -> str:
    return hashlib.sha1(norm(inp).encode()).hexdigest()[:16]


def main() -> None:
    gold_inputs = {}
    for line in open(GOLD):
        if line.strip():
            v = json.loads(line)
            gold_inputs[norm(v["input"])] = v["input"]

    cache = {}
    for line in open(CACHE):
        if not line.strip():
            continue
        r = json.loads(line)
        key = norm(r.get("input", ""))
        if key:
            # keep the record with a perturbation if duplicated
            if key in cache and cache[key].get("has_perturb") and not r.get("has_perturb"):
                continue
            cache[key] = r

    n = 0
    with open(OUT, "w") as f:
        for key, full_input in gold_inputs.items():
            r = cache.get(key)
            if r is None:
                continue
            rec = {
                "id": cid(full_input),
                "input": full_input,
                "raw": r["raw"],
                "orig_issue": r.get("orig_issue"),
                "praw": r.get("praw", ""),
                "praw_issue": r.get("praw_issue"),
                "has_perturb": r.get("has_perturb", False),
            }
            f.write(json.dumps(rec) + "\n")
            n += 1
    print(f"labelled decoded & joined: {n} / {len(gold_inputs)} -> {OUT}")
    print(f"missing decodes: {len(gold_inputs) - n}")


if __name__ == "__main__":
    main()
