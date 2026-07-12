import gzip, pickle, re, json
class Safe(pickle.Unpickler):
    def find_class(self, m, n):
        ok={('builtins','dict'),('builtins','list'),('builtins','set'),('builtins','tuple'),('builtins','str'),('builtins','int'),('builtins','float'),('builtins','frozenset'),('collections','OrderedDict'),('collections','defaultdict')}
        if (m,n) in ok: return super().find_class(m,n)
        raise pickle.UnpicklingError(m+"."+n)
def fold(s): return re.sub(r'[^a-z0-9]','',(s or '').lower())
fn=Safe(gzip.open('first_names.pkl.gz','rb')).load()
def minrank(a):
    r=a.get('rank',{}); return min(r.values()) if r else 10**9
T=1000
fore={fold(k) for k,v in fn.items() if minrank(v)<T and len(fold(k))>=2}
raws={r['id']:r for r in (json.loads(l) for l in open('../scratchpad_raws_test.jsonl') if l.strip())}
rows=[json.loads(l) for l in open('../scratchpad_p13.jsonl') if l.strip()]
def has_other(src, cred):
    toks=[t.strip('.,;:()[]"“”‘’|') for t in src.split()]; toks=[t for t in toks if t]
    isname=lambda t: t[:1].isupper() and sum(c.isalpha() for c in t)>=2
    isinit=lambda t: t[:1].isupper() and len(t.rstrip('.'))==1
    for i,t in enumerate(toks):
        if not isname(t) or fold(t) not in fore: continue
        j=i+1
        while j<len(toks) and isinit(toks[j]): j+=1
        if j>=len(toks) or not isname(toks[j]): continue
        last=fold(toks[j])
        if last not in cred and len(last)>=2: return True, t+' '+toks[j]
    return False, None
keep=canc=regr=0
for r in rows:
    if r['reason']!='et_al_undercount': continue
    src=raws.get(r['id'],{}).get('input','').replace('parse citation: ','')
    cred=[fold(x) for x in r['model_surnames']]
    if len(cred)==0: continue
    found,ex=has_other(src,cred); real=not r['correct']
    if real and found: keep+=1
    elif real and not found: regr+=1
    elif not real and not found: canc+=1
    tag='GOOD' if real else 'over'; act='KEEP' if found else 'CANCEL'
    star=' <-- REGRESSION' if (real and not found) else ''
    print("  [%s] %s m=%s found=%s%s" % (tag,act,r['model_surnames'],repr(ex),star))
print("\nSUMMARY (T=%d, given-names only): good-kept=%d over-cancelled=%d REGRESSIONS=%d" % (T,keep,canc,regr))
