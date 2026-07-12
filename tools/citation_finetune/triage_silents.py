#!/usr/bin/env python3
"""Bucket the silent failures from a replay-scored file into likely-real-bug vs likely-gold-error,
using source grounding (no extra model calls). A 'silent' = the flagger passed the cite but the
model author-surname set != gold.

Buckets:
  REAL_OVEREXTRACT : model has a surname absent from the source  -> repair fabricated it.
  REAL_OMISSION    : gold has a surname (present in source) the model missed -> repair dropped it.
  GOLD_EMPTY       : gold has no authors but the model's are grounded -> likely gold-label error.
  GOLD_UNGROUNDED  : a gold surname is absent from the source -> likely gold-label error.
  AMBIGUOUS        : disagreement where both sides are grounded (name-order, typo, convention).

Usage: triage_silents.py <scored.jsonl> [--show REAL_OVEREXTRACT,REAL_OMISSION] [--limit N]
"""
import json
import re
import sys
import unicodedata


ROOT = "/home/adam/Projects/flowstate"
_COMMON = {l.strip() for l in open(f"{ROOT}/crates/flowstate-citation/data/common_words.txt") if l.strip()}


def _common(s: str) -> bool:
    return s in _COMMON


def fold(s: str) -> str:
    s = unicodedata.normalize("NFKD", s)
    return "".join(c for c in s if c.isalnum()).lower()


def source_tokens(src: str) -> set[str]:
    src = src.replace("parse citation:", "")
    return {fold(t) for t in re.split(r"[\s,;:\[\]{}()\"']+", src) if fold(t)}


def grounded(surname: str, src_folded: str, toks: set[str]) -> bool:
    f = fold(surname)
    if len(f) < 2:
        return True  # can't judge a 1-char token
    if f in src_folded:
        return True
    # near-match to any source token (typo tolerance)
    for t in toks:
        if len(t) >= 3 and (f in t or t in f):
            return True
        if len(t) == len(f) and sum(a != b for a, b in zip(t, f)) <= 1:
            return True
    return False


def main() -> None:
    path = sys.argv[1]
    show = set()
    limit = 25
    i = 2
    while i < len(sys.argv):
        if sys.argv[i] == "--show":
            show = set(sys.argv[i + 1].split(","))
            i += 2
        elif sys.argv[i] == "--limit":
            limit = int(sys.argv[i + 1])
            i += 2
        else:
            i += 1

    buckets: dict[str, list] = {}
    for line in open(path):
        if not line.strip():
            continue
        r = json.loads(line)
        if not (r.get("passed") and not r.get("correct")):
            continue
        src = r.get("input", "")
        srcf = fold(src)
        toks = source_tokens(src)
        model = set(r.get("model_surnames", []))
        gold = set(r.get("gold_surnames", []))
        extra = model - gold   # model has, gold doesn't
        missed = gold - model   # gold has, model doesn't

        model_ungrounded = [s for s in extra if not grounded(s, srcf, toks)]
        gold_ungrounded = [s for s in gold if not grounded(s, srcf, toks)]
        missed_in_src = [s for s in missed if grounded(s, srcf, toks)]

        MONTHS = {"january", "february", "march", "april", "may", "june", "july",
                  "august", "september", "october", "november", "december"}
        # gold "surnames" that are obviously not names: months, or a lone common word / digit.
        gold_junk = [s for s in gold if s in MONTHS or s.isdigit()
                     or (len(gold) == 1 and _common(s))]
        # particle/compound: a model surname is a strict suffix of a gold compound (rythoven ⊂
        # vanrythoven) or vice-versa — the model dropped a leading particle / first component.
        particle = any(g != m and (g.endswith(m) or m.endswith(g)) and min(len(g), len(m)) >= 4
                       for g in missed for m in extra)
        # model typo: an extra surname is a near-spelling of a missed one (behrant ~ behrent).
        typo = any(g != m and len(g) >= 4 and len(m) >= 4
                   and sum(a != b for a, b in zip(g, m)) <= 2 and abs(len(g) - len(m)) <= 1
                   for g in missed for m in extra)

        if model_ungrounded:
            b = "REAL_OVEREXTRACT"
        elif not gold and model:
            b = "GOLD_EMPTY"
        elif gold_junk:
            b = "GOLD_JUNK"
        elif gold_ungrounded:
            b = "GOLD_UNGROUNDED"
        elif particle:
            b = "REAL_PARTICLE"
        elif typo:
            b = "MODEL_TYPO"
        elif missed_in_src:
            b = "REAL_OMISSION"
        else:
            b = "AMBIGUOUS"
        buckets.setdefault(b, []).append((r, sorted(extra), sorted(missed), model_ungrounded))

    total = sum(len(v) for v in buckets.values())
    order = ["REAL_OVEREXTRACT", "REAL_OMISSION", "REAL_PARTICLE", "MODEL_TYPO",
             "GOLD_EMPTY", "GOLD_JUNK", "GOLD_UNGROUNDED", "AMBIGUOUS"]
    real = sum(len(buckets.get(b, [])) for b in ["REAL_OVEREXTRACT", "REAL_OMISSION", "REAL_PARTICLE"])
    goldish = sum(len(buckets.get(b, [])) for b in ["GOLD_EMPTY", "GOLD_JUNK", "GOLD_UNGROUNDED"])
    print(f"=== {total} silents | REAL(fixable)~{real}  GOLD-ERROR~{goldish}  typo/ambig~{total - real - goldish} ===")
    for b in order:
        print(f"  {b}: {len(buckets.get(b, []))}")
    for b in show:
        print(f"\n===== {b} =====")
        for r, extra, missed, ung in buckets.get(b, [])[:limit]:
            print(f"--- {r['id']}")
            print(f"  input: {r['input'][:200]}")
            print(f"  model: {sorted(r.get('model_surnames', []))}")
            print(f"  gold : {sorted(r.get('gold_surnames', []))}")
            if extra:
                print(f"  extra (model-only): {extra}  ungrounded={ung}")
            if missed:
                print(f"  missed (gold-only): {missed}")


if __name__ == "__main__":
    main()
