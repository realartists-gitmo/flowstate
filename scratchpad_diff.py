import json, sys
base_path = sys.argv[1]   # old scored (full)
new_path = sys.argv[2]    # replay scored (partial ok)

def load(p):
    d={}
    for l in open(p):
        if not l.strip(): continue
        r=json.loads(l); d[r["id"]]=r
    return d
base=load(base_path); new=load(new_path)

def state(r):
    if r["passed"] and r["correct"]: return "perfect"
    if r["passed"] and not r["correct"]: return "SILENT"
    if not r["passed"] and not r["correct"]: return "good"
    return "over"

fixes=[]; regr=[]; other=[]
for gid,nr in new.items():
    if gid not in base: continue
    b=state(base[gid]); n=state(nr)
    if b==n: continue
    # improvement: SILENT->perfect, SILENT->good, over->perfect, over->good(?)
    rank={"perfect":0,"good":1,"over":2,"SILENT":3}
    if rank[n]<rank[b]: fixes.append((gid,b,n,nr))
    else: regr.append((gid,b,n,nr))

print(f"compared {sum(1 for g in new if g in base)} ids")
print(f"FIXES: {len(fixes)}   REGRESSIONS: {len(regr)}")
print("\n=== REGRESSIONS ===")
for gid,b,n,nr in regr:
    print(f"[{gid}] {b}->{n}  model={[a.get('surname') for a in (nr.get('model_authors') or [])]}")
    print(f"    gold={[a.get('surname') for a in (nr.get('gold_authors') or [])] if nr.get('gold_authors') else nr.get('gold_surnames')}")
    print(f"    reason={nr.get('reason')}  src={nr['input'][:150]}")
print("\n=== FIXES (transition counts) ===")
from collections import Counter
print(dict(Counter(f"{b}->{n}" for _,b,n,_ in fixes)))
print(dict(Counter(f"{b}->{n}" for _,b,n,_ in regr)), "<- regressions")
