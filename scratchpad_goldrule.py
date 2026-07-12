import json, re, sys
from collections import Counter

gold_path = "scratchpad_heldout.jsonl"
SUF = {"jr","sr","ii","iii","iv","v"}
def fold(s): return re.sub(r"[^a-z0-9]", "", (s or "").lower())

def last_sig(name):
    toks = [t for t in re.split(r"\s+", name.strip()) if t]
    # strip trailing generational suffix
    while len(toks) > 1 and fold(toks[-1]) in SUF:
        toks.pop()
    # strip trailing bare initials (single letter +/- dot)
    while len(toks) > 1 and re.fullmatch(r"[A-Za-z]\.?", toks[-1]):
        toks.pop()
    return toks[-1] if toks else ""

n=0; match=0; mism=[]
for l in open(gold_path):
    if not l.strip(): continue
    r=json.loads(l); tgt=r["target"]
    if isinstance(tgt,str):
        try: tgt=json.loads(tgt)
        except: continue
    for a in (tgt.get("authors") or []):
        sur=a.get("surname"); name=a.get("name")
        if not sur or not name: continue
        n+=1
        ls=last_sig(name)
        if fold(sur)==fold(ls): match+=1
        else:
            # also accept: surname is last token joined with a particle (de la Vega -> Vega already last)
            mism.append((r["id"],sur,name,ls))

print(f"authors with both fields: {n}")
print(f"surname == last significant token: {match} ({100*match/max(1,n):.1f}%)")
print(f"MISMATCHES: {len(mism)}")
# categorize mismatches
cats=Counter()
for gid,sur,name,ls in mism:
    fsur=fold(sur); fname=fold(name)
    if fsur not in fname: cats["surname_not_in_name"]+=1
    elif fname.startswith(fsur): cats["surname_is_FIRST_token"]+=1
    else: cats["surname_is_MIDDLE_token"]+=1
print("mismatch cats:", dict(cats))
print("\n--- sample surname_is_FIRST_token (surname-first order, MUST protect) ---")
shown=0
for gid,sur,name,ls in mism:
    if fold(name).startswith(fold(sur)) and fold(sur) in fold(name):
        print(f"  [{gid}] surname={sur!r} name={name!r} last={ls!r}"); shown+=1
        if shown>=25: break
