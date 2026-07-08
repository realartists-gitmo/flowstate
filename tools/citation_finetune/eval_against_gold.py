#!/usr/bin/env python3
"""Field-level eval of the heuristic labeler against the LLM gold set.

Runs draft_label over the held-out gold cites and compares its sparse target to
the gold sparse target, field by field, overall and per primary_stratum.  Field
accuracy — not exact-string match — is the headline metric, and reject accuracy
is tracked separately from parsed-field accuracy (per the project plan).
"""

from __future__ import annotations

import argparse
import json
import re
from collections import defaultdict
from pathlib import Path

import draft_label

DEFAULT_GOLD_SAMPLE = Path("datasets/citation_finetune/gold_sample.jsonl")
DEFAULT_GOLD_LABELS = Path("datasets/citation_finetune/gold_labeled.jsonl")

SCALAR_FIELDS = [
    "year", "no_date", "title", "publication", "url", "doi", "pages",
    "volume", "issue", "source_type", "published_date", "accessed_date",
]


def norm_str(v) -> str:
    if v is None:
        return ""
    return re.sub(r"[^a-z0-9]", "", str(v).lower())


def norm_title(v) -> str:
    return re.sub(r"[^a-z0-9]", "", (v or "").lower())


def tokens(v: str) -> set:
    return set(re.findall(r"[a-z0-9]+", (v or "").lower()))


class Tally:
    __slots__ = ("tp", "fp", "fn", "match", "both")

    def __init__(self):
        self.tp = self.fp = self.fn = self.match = self.both = 0

    def add(self, gold_present, pred_present, agree):
        if gold_present and pred_present:
            self.both += 1
            if agree:
                self.match += 1
        if gold_present:
            self.fn += 0 if pred_present else 1
        if pred_present and not gold_present:
            self.fp += 1
        if gold_present and pred_present:
            self.tp += 1

    def line(self, name):
        p = self.tp / (self.tp + self.fp) if (self.tp + self.fp) else 1.0
        r = self.tp / (self.tp + self.fn) if (self.tp + self.fn) else 1.0
        acc = self.match / self.both if self.both else 1.0
        f1 = 2 * p * r / (p + r) if (p + r) else 0.0
        return f"  {name:16} presence P/R/F1={p:.2f}/{r:.2f}/{f1:.2f}  value-acc={acc:.2f} (n={self.both})"


def author_score(gold_authors, pred_authors):
    """Return (family_correct, given_correct, qual_f1, count_exact) over aligned authors."""
    gi = {norm_str(a.get("family")): a for a in gold_authors if a.get("family")}
    fam_ok = given_ok = 0
    qual_f1_sum = 0.0
    qual_n = 0
    matched = 0
    for pa in pred_authors:
        key = norm_str(pa.get("family"))
        ga = gi.get(key)
        if not ga:
            continue
        matched += 1
        fam_ok += 1
        if norm_str(pa.get("given")) == norm_str(ga.get("given")) and ga.get("given"):
            given_ok += 1
        gq = " ".join(ga.get("qualifications") or [])
        pq = " ".join(pa.get("qualifications") or [])
        if gq or pq:
            tg, tp = tokens(gq), tokens(pq)
            if tg and tp:
                inter = len(tg & tp)
                prec = inter / len(tp)
                rec = inter / len(tg)
                qual_f1_sum += (2 * prec * rec / (prec + rec)) if (prec + rec) else 0.0
            qual_n += 1
    return {
        "matched": matched,
        "gold_n": len(gold_authors),
        "pred_n": len(pred_authors),
        "family_ok": fam_ok,
        "given_ok": given_ok,
        "given_gold": sum(1 for a in gold_authors if a.get("given")),
        "qual_f1_sum": qual_f1_sum,
        "qual_n": qual_n,
        "count_exact": int(len(gold_authors) == len(pred_authors)),
    }


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--gold-sample", type=Path, default=DEFAULT_GOLD_SAMPLE)
    ap.add_argument("--gold-labels", type=Path, default=DEFAULT_GOLD_LABELS)
    ap.add_argument("--out", type=Path, default=Path("datasets/citation_finetune/gold_eval_report.json"))
    args = ap.parse_args()

    cites = {r["id"]: r for r in (json.loads(l) for l in args.gold_sample.open(encoding="utf-8"))}
    gold = {}
    for line in args.gold_labels.open(encoding="utf-8"):
        r = json.loads(line)
        gold[r["id"]] = r["target"]

    ids = [i for i in cites if i in gold]
    scalar = {f: Tally() for f in SCALAR_FIELDS}
    scalar_by_stratum = defaultdict(lambda: {f: Tally() for f in SCALAR_FIELDS})
    status_correct = 0
    reject_gold = reject_pred = reject_both_correct = 0
    parsed_pairs = 0
    author_agg = defaultdict(float)

    for i in ids:
        row = cites[i]
        heur = draft_label.to_target(draft_label.label_row(
            {"fullcite": row["fullcite"], "primary_stratum": row["primary_stratum"], "is_likely_reject": row.get("is_likely_reject", False)}
        ))
        g = gold[i]
        strat = row["primary_stratum"]
        if heur.get("status") == g.get("status"):
            status_correct += 1
        if g.get("status") == "reject":
            reject_gold += 1
            if heur.get("status") == "reject":
                reject_both_correct += 1
        if heur.get("status") == "reject":
            reject_pred += 1
        if g.get("status") != "parsed" or heur.get("status") != "parsed":
            continue
        parsed_pairs += 1
        for f in SCALAR_FIELDS:
            gv, pv = g.get(f), heur.get(f)
            gp, pp = gv not in (None, "", [], False), pv not in (None, "", [], False)
            if f == "title":
                agree = norm_title(gv) == norm_title(pv) or (tokens(gv) and tokens(pv) and len(tokens(gv) & tokens(pv)) / max(1, len(tokens(gv) | tokens(pv))) > 0.6)
            elif f == "year":
                agree = gv == pv
            elif f in ("published_date", "accessed_date"):
                # gold dates are ISO, heuristic keeps raw text — compare by year.
                agree = draft_label.year_from_date(str(gv)) == draft_label.year_from_date(str(pv))
            else:
                agree = norm_str(gv) == norm_str(pv)
            scalar[f].add(gp, pp, agree)
            scalar_by_stratum[strat][f].add(gp, pp, agree)
        a = author_score(g.get("authors", []), heur.get("authors", []))
        for k, v in a.items():
            author_agg[k] += v

    report = {
        "n_gold": len(gold),
        "n_evaluated": len(ids),
        "status_accuracy": round(status_correct / len(ids), 3) if ids else None,
        "reject_recall": round(reject_both_correct / reject_gold, 3) if reject_gold else None,
        "reject_gold_count": reject_gold,
        "parsed_pairs": parsed_pairs,
        "author": {
            "count_exact_rate": round(author_agg["count_exact"] / parsed_pairs, 3) if parsed_pairs else None,
            "family_match_of_pred": round(author_agg["family_ok"] / author_agg["pred_n"], 3) if author_agg["pred_n"] else None,
            "family_recall_of_gold": round(author_agg["family_ok"] / author_agg["gold_n"], 3) if author_agg["gold_n"] else None,
            "given_acc_where_gold_has": round(author_agg["given_ok"] / author_agg["given_gold"], 3) if author_agg["given_gold"] else None,
            "qual_token_f1": round(author_agg["qual_f1_sum"] / author_agg["qual_n"], 3) if author_agg["qual_n"] else None,
        },
        "scalar_fields": {},
    }

    print(f"== Gold eval ==  n_gold={len(gold)}  evaluated={len(ids)}")
    print(f"status accuracy:      {report['status_accuracy']}")
    print(f"reject recall:        {report['reject_recall']}  (gold rejects={reject_gold})")
    print(f"parsed<->parsed pairs: {parsed_pairs}")
    print("\n-- authors --")
    for k, v in report["author"].items():
        print(f"  {k:26} {v}")
    print("\n-- scalar fields (presence P/R/F1, value-acc where both present) --")
    for f in SCALAR_FIELDS:
        print(scalar[f].line(f))
        t = scalar[f]
        report["scalar_fields"][f] = {
            "presence_f1": round(2 * (t.tp / (t.tp + t.fp) if (t.tp + t.fp) else 1) * (t.tp / (t.tp + t.fn) if (t.tp + t.fn) else 1) / max(1e-9, (t.tp / (t.tp + t.fp) if (t.tp + t.fp) else 1) + (t.tp / (t.tp + t.fn) if (t.tp + t.fn) else 1)), 3),
            "value_acc": round(t.match / t.both, 3) if t.both else None,
            "n_both": t.both,
        }

    args.out.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(f"\nWrote {args.out}")


if __name__ == "__main__":
    main()
