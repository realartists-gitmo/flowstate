# Citation relabel run — results

Frozen new-format relabel of the labeled corpus via blind 2-candidate LLM adjudication
(source + model output + Haiku label, unlabeled A/B). See `relabel_spec.md` for the rubric.

## Corpus reconciliation
- **Total labeled corpus:** 5,700 (`datasets/citation_finetune/labels_full_draft.jsonl`)
- **Fully resolved this run:** 2,876 (all ids ⊂ full corpus)
  - Adjudicated (buckets 1+2 disagreements/hard-class, + validation/org pulls): 1,411
  - Trusted (bucket 3 easy agreements, validated safe): 1,465
- **Remaining:** 2,824 — have Haiku labels but no model output yet (int8 cache was cut at 2,876).

## Adjudication result — "model beats label" holds at full scale
On the 1,411 hard/disagreement pool, which candidate the blind adjudicator sided with per field:

| field       | model | haiku | neither (both wrong) |
|-------------|------:|------:|---------------------:|
| authors     |  378  |  343  | 160 |
| title       |  411  |  363  | 137 |
| source_type |  354  |  414  | 120 |

- Model beats the Haiku label on **authors** and **title** (the `title=None` error class);
  Haiku edges it only on **source_type** (the ambiguous field).
- **~10% "neither"** per field — both candidates were wrong, adjudicator produced a fresh answer.
  This is the direct payoff of the relabel pass.

## Trusted-set validation (answers "was the audit exhaustive?")
Detector-free 150-cite blind re-adjudication of trusted (model≡Haiku) agreements:
- **Override rate: 0.7% (1/150)** — 0 surname errors, 0 title errors, 1 source_type (report→news).
- Confirms the 1,465 trusted agreements are safe to keep without a full relabel.

## Output shape (frozen new format)
`final_training.jsonl` — 2,876 rows `{id, source, target, needs_human, prov}`:
- `target`: `{status, authors:[{surname,name}], title, source_type, publisher, publication,
  container_title, year, published_date, url, doi, volume, issue, pages, spillover_start_text}`
- Format changes vs old Haiku labels: single `surname`+`name` (no family/given split);
  `spillover_start_text` string (no index); status ∈ {parsed, reject}.
- Status: 2,599 parsed / 277 reject (10 empty fragments forced to reject).
- **needs_human: 99** — genuine unresolvables (bare cite tags, truncated sources,
  first-name-only, dictionary abbreviations). Surface, do not train blindly.

## Orchestration
Three agents in parallel: Codex (gpt-5.4-mini ×24, bulk), Claude Workflow (Sonnet lower-half,
544/544 clean), Grok 4.5 (150-cite validation). 0 malformed outputs across 1,411 jobs.
