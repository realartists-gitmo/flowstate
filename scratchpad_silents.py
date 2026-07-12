import json, sys, re
from collections import Counter, defaultdict

scored = sys.argv[1]
gold_path = sys.argv[2] if len(sys.argv) > 2 else "scratchpad_heldout.jsonl"

# full gold authors by id
gold = {}
for l in open(gold_path):
    if not l.strip(): continue
    r = json.loads(l)
    tgt = r["target"]
    if isinstance(tgt, str):
        try: tgt = json.loads(tgt)
        except: tgt = {}
    gold[r["id"]] = tgt

def fold(s):
    return re.sub(r"[^a-z0-9]", "", (s or "").lower())

rows = [json.loads(l) for l in open(scored) if l.strip()]
sils = [r for r in rows if r["passed"] and not r["correct"]]
print(f"total rows {len(rows)}  silents {len(sils)}")

def kind(r):
    m = set(x.lower() for x in r["model_surnames"])
    g = set(x.lower() for x in r["gold_surnames"])
    if m < g: return "omission"
    if m > g: return "over-extract"
    return "corruption/disjoint"

print("taxonomy:", dict(Counter(kind(r) for r in sils)))
print()

for k in ["corruption/disjoint", "omission", "over-extract"]:
    grp = [r for r in sils if kind(r) == k]
    print(f"\n===== {k}  ({len(grp)}) =====")
    for r in grp:
        gid = r["id"]
        m = set(x.lower() for x in r["model_surnames"])
        g = set(x.lower() for x in r["gold_surnames"])
        missing = g - m
        extra = m - g
        ga = gold.get(gid, {}).get("authors", [])
        gnames = [(a.get("surname"), a.get("name")) for a in ga] if isinstance(ga, list) else []
        ma = r.get("model_authors") or []
        mnames = [(a.get("surname"), a.get("name")) for a in ma] if isinstance(ma, list) else []
        inp = r["input"].replace("parse citation:", "").strip()
        print(f"\n[{gid}] miss={sorted(missing)} extra={sorted(extra)}")
        print(f"  GOLD : {gnames}")
        print(f"  MODEL: {mnames}")
        print(f"  SRC  : {inp[:240]}")
