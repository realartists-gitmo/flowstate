import gzip, pickle, re
class Safe(pickle.Unpickler):
    def find_class(self, m, n):
        ok={('builtins','dict'),('builtins','list'),('builtins','set'),('builtins','tuple'),('builtins','str'),('builtins','int'),('builtins','float'),('builtins','frozenset'),('collections','OrderedDict'),('collections','defaultdict')}
        if (m,n) in ok: return super().find_class(m,n)
        raise pickle.UnpicklingError(m+"."+n)
def fold(s): return re.sub(r'[^a-z0-9]','',(s or '').lower())
def minrank(a):
    r=a.get('rank',{}); return min(r.values()) if r else 10**9
fn=Safe(gzip.open('first_names.pkl.gz','rb')).load()
ln=Safe(gzip.open('last_names.pkl.gz','rb')).load()
fore=sorted({fold(k) for k,v in fn.items() if minrank(v)<1000 and len(fold(k))>=2})
sur=sorted({fold(k) for k,v in ln.items() if minrank(v)<2000 and len(fold(k))>=2})
import os
os.makedirs('../crates/flowstate-citation/data', exist_ok=True)
open('../crates/flowstate-citation/data/given_names.txt','w').write('\n'.join(fore)+'\n')
open('../crates/flowstate-citation/data/surnames.txt','w').write('\n'.join(sur)+'\n')
print('given_names.txt:', len(fore), 'surnames.txt:', len(sur))
