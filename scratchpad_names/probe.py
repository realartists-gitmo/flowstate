import gzip, pickle, re
class Safe(pickle.Unpickler):
    def find_class(self, m, n):
        ok = {('builtins','dict'),('builtins','list'),('builtins','set'),('builtins','tuple'),('builtins','str'),('builtins','int'),('builtins','float'),('builtins','frozenset'),('collections','OrderedDict'),('collections','defaultdict')}
        if (m,n) in ok: return super().find_class(m,n)
        raise pickle.UnpicklingError(m+"."+n)
def fold(s): return re.sub(r'[^a-z0-9]','',(s or '').lower())
fn = Safe(gzip.open('first_names.pkl.gz','rb')).load()
def minrank(a):
    r=a.get('rank',{}); return min(r.values()) if r else 10**9
print("min-rank of tokens:")
for t in ['Timothy','Drew','Armin','Bailey','Erez','Ananya','Matthew','James','New','San','Harvard','Dr','Mexico','Business','Economics','Social']:
    a=fn.get(t)
    print("  %-10s %s" % (t, (minrank(a) if a else 'ABSENT')))
for T in [1000,2000,5000]:
    s={fold(k) for k,v in fn.items() if minrank(v)<T and len(fold(k))>=2}
    hits=[n for n in ['timothy','drew','armin','bailey','erez','ananya'] if n in s]
    noise=[n for n in ['new','san','harvard','dr'] if n in s]
    print("T=%d: %d names | real-kept=%s | noise-in=%s" % (T,len(s),hits,noise))
