# ============================================================================
# Flan-T5 citation->JSON fine-tune (Colab, A100 High-RAM)
# Upload to the Colab session: train.jsonl, eval.jsonl, test.jsonl, eval_hardballs.jsonl
#   (from datasets/citation_finetune/splits_v6/)
# Target is the SPARSE JSON string; T5's SentencePiece vocab drops { } so the model
# learns the brace-free form (reconstruct.rs re-inserts braces at inference).
# ============================================================================
import json, re, os, math, random
import torch
from datasets import Dataset
from transformers import (AutoTokenizer, AutoModelForSeq2SeqLM, DataCollatorForSeq2Seq,
                          Seq2SeqTrainer, Seq2SeqTrainingArguments)

MODEL   = os.environ.get("MODEL", "google/flan-t5-small")   # or google/flan-t5-base
DATA    = os.environ.get("DATA", "/content")                # where the 4 jsonl live
EPOCHS  = int(os.environ.get("EPOCHS", "18"))
MAXSRC  = int(os.environ.get("MAXSRC", "3072"))
MAXTGT  = int(os.environ.get("MAXTGT", "3072"))             # production checkpoint ceiling
BS      = int(os.environ.get("BS", "32"))
LR      = float(os.environ.get("LR", "3e-4"))
OUT     = os.environ.get("OUT", f"/content/ckpt_{MODEL.split('/')[-1]}")

def load(f):
    return [json.loads(l) for l in open(os.path.join(DATA, f))]
train, ev, test, hard = load("train.jsonl"), load("eval.jsonl"), load("test.jsonl"), load("eval_hardballs.jsonl")
print(f"train {len(train)}  eval {len(ev)}  test {len(test)}  hardballs {len(hard)}")

tok = AutoTokenizer.from_pretrained(MODEL)
def prep(rows):
    ds = Dataset.from_list([{"input": r["input"], "target": r["target"]} for r in rows])
    def f(b):
        m = tok(b["input"], max_length=MAXSRC, truncation=True)
        with tok.as_target_tokenizer():
            lab = tok(b["target"], max_length=MAXTGT, truncation=True)
        m["labels"] = lab["input_ids"]; return m
    return ds.map(f, batched=True, remove_columns=["input","target"])
train_ds, eval_ds = prep(train), prep(ev)

# --- token-length sanity (so you SEE if MAXTGT is truncating the mega-cites) ---
tl = [len(tok(r["target"]).input_ids) for r in train]
over = sum(1 for x in tl if x > MAXTGT)
print(f"target tokens: p50={sorted(tl)[len(tl)//2]} p99={sorted(tl)[int(len(tl)*.99)]} max={max(tl)} | >{MAXTGT}toks(truncated): {over}")

model = AutoModelForSeq2SeqLM.from_pretrained(MODEL)
args = Seq2SeqTrainingArguments(
    output_dir=OUT, num_train_epochs=EPOCHS, learning_rate=LR,
    per_device_train_batch_size=BS, per_device_eval_batch_size=BS,
    bf16=True, logging_steps=50, eval_strategy="epoch", save_strategy="epoch",
    save_total_limit=2, predict_with_generate=True, generation_max_length=MAXTGT,
    load_best_model_at_end=True, metric_for_best_model="eval_loss", report_to="none",
    warmup_ratio=0.03, weight_decay=0.01,
)
collator = DataCollatorForSeq2Seq(tok, model=model)
trainer = Seq2SeqTrainer(model=model, args=args, train_dataset=train_ds, eval_dataset=eval_ds,
                         data_collator=collator, tokenizer=tok)
trainer.train()
trainer.save_model(OUT); tok.save_pretrained(OUT)

# ---- brace-free field-level eval (no full brace reconstruction needed) ----
def gen(rows, bs=64):
    outs=[]
    model.eval()
    for i in range(0, len(rows), bs):
        chunk=[r["input"] for r in rows[i:i+bs]]
        enc=tok(chunk, return_tensors="pt", padding=True, truncation=True, max_length=MAXSRC).to(model.device)
        with torch.no_grad():
            g=model.generate(**enc, max_length=MAXTGT, num_beams=1)
        outs+=tok.batch_decode(g, skip_special_tokens=True)
    return outs
def surnames(s):  # extract surname values from brace-free (or braced) output
    return set(x.lower() for x in re.findall(r'"surname":\s*"([^"]+)"', s))
def field(s, k):
    m=re.search(r'"'+k+r'":\s*"?([^",}\]]+)', s); return (m.group(1).strip().lower() if m else None)
def score(rows, tag):
    preds=gen(rows)
    n=len(rows); st=0; sf1=0.0; yr=0; styp=0; exact=0
    per=dict()
    for r,p in zip(rows,preds):
        g=r["target"]; cls=r.get("cls","real")
        gs, ps = surnames(g), surnames(p)
        tp=len(gs&ps); prec=tp/len(ps) if ps else (1 if not gs else 0); rec=tp/len(gs) if gs else (1 if not ps else 0)
        f1=2*prec*rec/(prec+rec) if (prec+rec) else (1.0 if not gs and not ps else 0.0)
        sf1+=f1
        if field(g,"status")==field(p,"status"): st+=1
        if field(g,"year")==field(p,"year"): yr+=1
        if field(g,"source_type")==field(p,"source_type"): styp+=1
        if gs==ps: exact+=1
        d=per.setdefault(cls,[0,0.0,0]); d[0]+=1; d[1]+=f1; d[2]+=(1 if gs==ps else 0)
    print(f"\n[{tag}] n={n}  status_acc={st/n:.3f}  author_setF1={sf1/n:.3f}  author_exact={exact/n:.3f}  year_acc={yr/n:.3f}  src_type_acc={styp/n:.3f}")
    if any(k!='real' for k in per):
        print("  per-class author_setF1 / exact:")
        for cls,d in sorted(per.items()): print(f"    {cls:18} n={d[0]:3d}  F1={d[1]/d[0]:.3f}  exact={d[2]/d[0]:.3f}")
score(ev, "REAL eval")
score(hard, "HARDBALLS (per-class)")
print(f"\nsaved model -> {OUT}")
