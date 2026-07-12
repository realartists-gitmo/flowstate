#!/usr/bin/env python3
"""Re-score the silent cites against the Sonnet-cleaned gold instead of the noisy train gold.

For every cite that has a clean label, compare the model's surname set to the clean set:
  MATCH        : model == clean            -> NOT a real silent (train gold was wrong).
  REAL_PARTICLE: model dropped a particle/compound component the clean surname keeps.
  REAL_PERSON  : model missed / mis-took a human author whose name is in the source.
  REAL_ORG     : clean gold is an organizational author the model returned empty for.
  REAL_OVER    : model produced a surname the clean gold does not (fabrication).
  AMBIGUOUS    : clean label type == ambiguous -> excluded from the real-silent count.

Usage: clean_rescore.py <scored.jsonl> <clean.jsonl> [--show CATEGORY] [--limit N]
"""
import json
import sys


def main() -> None:
    scored_path, clean_path = sys.argv[1], sys.argv[2]
    show, limit = set(), 40
    i = 3
    while i < len(sys.argv):
        if sys.argv[i] == "--show":
            show = set(sys.argv[i + 1].split(",")); i += 2
        elif sys.argv[i] == "--limit":
            limit = int(sys.argv[i + 1]); i += 2
        else:
            i += 1

    clean = {}
    for l in open(clean_path):
        if l.strip():
            r = json.loads(l)
            clean[r["id"]] = r
    scored = {json.loads(l)["id"]: json.loads(l) for l in open(scored_path) if l.strip()}

    buckets = {}
    for cid, cl in clean.items():
        s = scored.get(cid)
        if s is None:
            continue
        model = set(s.get("model_surnames", []))
        gold = set(cl["surnames"])
        typ = cl.get("type", "person")
        if model == gold:
            cat = "MATCH"
        elif typ == "ambiguous":
            cat = "AMBIGUOUS"
        else:
            extra = model - gold
            missed = gold - model
            particle = any(g != m and (g.endswith(m) or m.endswith(g)) and min(len(g), len(m)) >= 4
                           for g in missed for m in extra)
            if particle:
                cat = "REAL_PARTICLE"
            elif extra and not missed:
                cat = "REAL_OVER"
            elif typ == "org":
                cat = "REAL_ORG"
            else:
                cat = "REAL_PERSON"
        buckets.setdefault(cat, []).append((cid, s, cl))

    order = ["MATCH", "REAL_PARTICLE", "REAL_PERSON", "REAL_ORG", "REAL_OVER", "AMBIGUOUS"]
    total = sum(len(v) for v in buckets.values())
    real = sum(len(buckets.get(c, [])) for c in ["REAL_PARTICLE", "REAL_PERSON", "REAL_ORG", "REAL_OVER"])
    print(f"=== {total} relabeled | MATCH(train-gold-error)={len(buckets.get('MATCH', []))}  "
          f"REAL silent={real}  AMBIGUOUS={len(buckets.get('AMBIGUOUS', []))} ===")
    for c in order:
        print(f"  {c}: {len(buckets.get(c, []))}")
    for c in show:
        print(f"\n===== {c} =====")
        for cid, s, cl in buckets.get(c, [])[:limit]:
            print(f"  {s['input'][:95]}")
            print(f"     model={sorted(s.get('model_surnames', []))}  clean={sorted(cl['surnames'])} ({cl.get('type')})")


if __name__ == "__main__":
    main()
