import json, sys
from collections import Counter
path = sys.argv[1]
rows = [json.loads(l) for l in open(path) if l.strip()]
N = len(rows)
p=s=g=o=0; sils=[]; overs=[]
for r in rows:
    correct=r['correct']; passed=r['passed']
    if passed and correct: p+=1
    elif passed and not correct: s+=1; sils.append(r)
    elif not passed and not correct: g+=1
    else: o+=1; overs.append(r)
pct=lambda x: 100.0*x/max(1,N)
print(f"=== {path}  ({N} cites) ===")
print(f"PASSED & correct (perfect):  {p} ({pct(p):.1f}%)")
print(f"PASSED & wrong  (SILENT):    {s} ({pct(s):.1f}%)")
print(f"FLAGGED & wrong (good):      {g}")
print(f"FLAGGED & correct(over):     {o} ({pct(o):.1f}%)")
print(f"total flagged: {g+o} ({pct(g+o):.1f}%) | precision {100*g/max(1,g+o):.0f}%")
print(f"all flag reasons:  {dict(Counter(r['reason'] for r in rows if not r['passed']))}")
print(f"OVER-flag reasons: {dict(Counter(r['reason'] for r in overs))}  <-- which signals over-fire")
print(f"good-flag reasons: {dict(Counter(r['reason'] for r in rows if not r['passed'] and not r['correct']))}")
# silent taxonomy (omission vs over-extract vs corruption)
def kind(r):
    m=set(x.lower() for x in r['model_surnames']); gg=set(x.lower() for x in r['gold_surnames'])
    return 'omission' if m<gg else ('over-extract' if m>gg else 'corruption/disjoint')
print(f"SILENT taxonomy:   {dict(Counter(kind(r) for r in sils))}")
