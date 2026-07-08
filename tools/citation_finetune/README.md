# Citation fine-tune prep

Utilities for building a stratified citation parsing dataset from the local
OpenDebateEvidence DuckDB export.

## Source

Expected source database:

```text
/home/adam/datasets/OpenDebateEvidenceBM25DuckDB/opendebateevidence_good.duckdb
```

Expected table:

```text
bm25_tables.documents
```

The useful input field is `fullcite`. It is non-empty for every row in the
source table, but it includes short/incomplete cites, long spillover rows, URL
cites, database cites, and debate-specific annotations.

## Generate a sample

```sh
python tools/citation_finetune/stratified_sample.py
```

By default this writes:

```text
datasets/citation_finetune/stratified_sample.csv
datasets/citation_finetune/stratified_sample.jsonl
datasets/citation_finetune/weird_cases.csv
datasets/citation_finetune/stratified_sample_manifest.json
```

The default target is 5,700 rows, split across 19 citation-quality strata:
`likely_reject`, `very_short`, `short`, `very_long_spillover`, `long`,
`no_year_or_date`, `multiple_date_signals`, `no_date_label`, `card_signature`,
`doi`, `pages`, `published_date`, `lexis`, `jstor`, `accessed_or_doa`, `et_al`,
`author_qualifications`, `url`, and `ordinary`. Use
`--per-stratum` for a larger/smaller review set:

```sh
python tools/citation_finetune/stratified_sample.py --per-stratum 500
```

## Pipeline

```sh
# 1. sample (balanced 300/stratum training pool)
python tools/citation_finetune/stratified_sample.py

# 2. heuristic first-pass labels (head/body/tail structural parser)
python tools/citation_finetune/draft_label.py

# 3. fold in manual corrections (optional)
PYTHONPATH=tools/citation_finetune python tools/citation_finetune/apply_manual_overlay.py

# 4. build the sparse seq2seq target JSONL
#    target precedence per id: manual overlay > LLM relabel > heuristic
PYTHONPATH=tools/citation_finetune python tools/citation_finetune/make_training_jsonl.py

# 5. deterministic per-stratum train/eval/test split
python tools/citation_finetune/split_dataset.py
```

### Evaluation (held-out gold)

`build_gold_sample.py` draws a **natural-distribution** sample from the DuckDB,
excluding the training pool (no leakage). Those rows are LLM-labeled (Sonnet) to
form `gold_labeled.jsonl`, and `eval_against_gold.py` scores the heuristic
parser field-by-field (field accuracy, not exact-string; reject accuracy tracked
separately). See `gold_eval_report.json`.

### LLM relabel

The heuristic is strong on bibliographic fields (year/title/url/doi/pages/volume
≈ 0.9+ vs gold) but weak on authors (enumeration + given names), `publication`,
and `source_type`. `llm_relabel.jsonl` (`{id, target}`) holds Sonnet relabels for
the author-heavy strata; `make_training_jsonl.py` overrides the heuristic target
with these per id. The heuristic remains the fallback/validator.

## Label shape

The model emits a **sparse** structured target — only populated fields appear
(absent == null/empty), keys in a fixed canonical order, and labeler bookkeeping
(`confidence`, `inferred_fields`) is dropped from the target. See
`target_schema.json` for the exact training-target schema and `label_schema.json`
for the fuller internal label (which keeps bookkeeping under `_meta`).

Real citations use `status: "parsed"`. Inputs that are not citations use
`status: "reject"` and a `reject_reason` instead of forcing fake bibliographic
fields. Missing fields should only be inferred when the inference is valid from
the cite itself. For example, `Goodwin 12` may infer `year: 2012`, while a cite
with conflicting dates should preserve the conflict in `warnings`.

The sampler treats early bare one- or two-digit numbers as debate-style year
signals. This catches cites like `Amdur 14` and `Li 9` without treating every
page number in the citation as a publication year.

`weird_cases.csv` is not a training set. It is a small report of examples where
heuristics are likely to need human interpretation, such as possible signatures
that might be source suffixes, multiple date signals, huge single-paragraph
spillover, or no-year cites without an explicit "no date" label.

Sparse target shape (the exact string the model is trained to produce). A thin
bare cite `Goodwin 12`:

```json
{"status":"parsed","authors":[{"family":"Goodwin","literal":"Goodwin"}],"year":2012,"source_type":"unknown","warnings":["incomplete_citation"]}
```

A richer cite carries only the fields it actually populates, e.g.:

```json
{"status":"parsed","authors":[{"family":"Moen","given":"Ole Martin","literal":"Ole Martin Moen","qualifications":["Research Fellow in Philosophy at University of Oslo"]}],"year":2016,"title":"An Argument for Hedonism","publication":"Journal of Value Inquiry","source_type":"journal_article","card_signatures":["TDI"]}
```

Field-level evaluation should matter more than exact JSON text matching.
