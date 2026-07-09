# Citation model — frozen output contract (Phase 0)

**Status: DRAFT for review → freeze before any training run.**

This is the single source of truth consumed by (a) the Python label/target generator,
(b) the Rust JSON reconstructor, (c) the Rust constrained decoder, and (d) the eval
harness. **Anything in this document that changes after training forces a retrain.**
Constrained decoding and quantization are the only things deferred to *after* training.

Change-control rule:
- **Pre-training (locked here):** target format, field set, enum vocabularies, source_type
  rubric, reject/bare-cite policy, author rules, tokenization, max lengths, labeling policy.
- **Post-training (safe to iterate):** the constrained-decoding grammar *implementation*
  (must match this spec), quantization, the Candle inference stack, deterministic-override
  merge code.

---

## 1. Model & task

- Task prefix (input): `parse citation: <fullcite>`
- Model: Flan-T5 **Small (80M)** and **Base (248M)** trained on identical frozen data;
  pick the smaller that clears the quality bar (decided empirically in Phase 6).
- Output: a **sparse, brace-free** serialization of the citation object (see §3), which
  Rust reconstructs into JSON (see §4).

**Deployment (Candle — confirmed viable by de-risk spike):**
- `candle-transformers` has a real encoder-decoder `t5::T5ForConditionalGeneration`
  (cross-attention + KV cache) and a `quantized_t5` variant; Flan-T5 loads from safetensors;
  greedy decode (no beam search — fine for us; we own the decode loop).
- **Quantization: GGUF q6k/q4k supported for T5.** Weight-file sizes: **Small q6k ≈ 60 MB**
  (q4k ≈ 40 MB), **Base q6k ≈ 260 MB**. Binary code is low-tens-of-MB (lean CPU backend,
  no CUDA/BLAS); the model file dominates footprint. Convert our fine-tuned checkpoint with
  `tensor-tools quantize --quantization q6k`.
- **Constrained decoding: `llguidance` (MIT)** produces per-step allowed-token masks from a
  JSON-Schema / Lark grammar over the T5 `tokenizer.json`; apply the mask to the logits
  tensor before sampling (~5 lines in our decode loop). Avoids a fully hand-rolled state
  machine. (DIY masker remains a fallback.)

## 2. Serialization format (why brace-free)

T5's SentencePiece vocab has **no `{` or `}`** — they decode to nothing, so the model is
trained to emit everything *except* object braces, and Rust re-inserts them
deterministically. `[ ] " : ,` all survive the vocab intact. This is proven (test-split
valid-JSON 0% → 95%+ after reconstruction). **Do not** add brace tokens + resize
embeddings — it destabilizes the lm_head (val loss 0.08 → 2.2).

## 3. Target schema (sparse — only populated fields appear)

Canonical key order (a field is emitted only when populated; `status` always present;
`source_type` always present for parsed):

```
status, authors, year, no_date, published_date, accessed_date, retrieved_date,
title, container_title, publication, publisher, volume, issue, pages, url, doi,
database, source_type, card_signatures, debate_annotations, raw_tail,
spillover_start_index, spillover_start_text, warnings          (reject: reject_reason, evidence)
```

Author object key order: `family, given, literal, qualifications`. `qualifications` is a
**single-element** array (one blob string). Target-side caps: **≤12 authors**, each
**qualification ≤400 chars** (keeps mega-cites learnable). See `target_schema.json` for
the machine-readable version; keep it in sync with this doc.

## 4. Reconstruction rules (Rust)

Only OBJECT braces are missing; everything else is intact.
- Wrap the whole output in `{ }` (top-level object).
- The `authors` value is `[ ... ]`; split it into author objects — a new author begins at
  each top-level `"family"` (first key of every author object) — and wrap each in `{ }`.
- Arrays of strings (`warnings`, `card_signatures`, `qualifications`) already carry `[ ]`.
- Quote/escape/bracket-depth aware scanning (reference impl in the notebook `reconstruct`).

## 5. Field set & the deterministic layer (validate/supplement — NOT override)

**The model is the source of truth for EVERY field.** It emits the full structure; nothing is
dropped from its output. The deterministic Rust layer only **validates/supplements** — it may
fill a field the model left empty, or flag a mismatch — but it does **not clobber** a model
value, because in messy debate cites the "mechanical" fields are *not* reliably
regex-recoverable (mangled/line-split URLs, varied DOI and page/volume formats). Any place we
later let the regex win must be justified by measured evidence that it beats the model on that
field; default is model-wins.

*Rejected (see `deployment_protocols.md`):* the "reduced target" — dropping
url/doi/pages/volume/issue/dates from the model's output and regex-only recovering them. They
are not dependably regex-recoverable, so the model keeps emitting them.

*Adopted format change (protocol 5):* **compact serialization** (short field keys) to shorten
every output uniformly — a frozen-format change to define and bundle into the production
retrain (update this doc, `target_schema.json`, and the Rust reconstructor/grammar together).

Latency/serving is governed by `deployment_protocols.md` (equity mandate: usable on any
hardware; no per-cite fast paths).

## 6. Enum vocabularies (hard constraints for constrained decoding)

- **source_type**: `journal_article, law_review, news_article, web_page, book,
  book_chapter, report, thesis, legal_source, dictionary_or_reference, interview, unknown`
- **reject_reason**: `not_a_citation, analytic_or_tag, cross_reference_only,
  too_malformed, empty_or_placeholder`
- **warnings (model target set): `body_spillover` only** (§8b). The full set —
  `incomplete_citation, url_only, conflicting_dates, source_type_ambiguous, et_al, no_date,
  body_spillover` — survives as the **deterministic layer's** output vocabulary, computed at
  inference, not predicted by the model.

The constrained decoder MUST restrict these positions to exactly these tokens.

## 7. source_type decision tree (NEW — resolves the ambiguity the Haiku experiment surfaced)

The Haiku-vs-Sonnet gap was concentrated in `source_type` (0.86) because the boundary was
underspecified — *not* a model weakness. Apply **first match wins, top to bottom**:

1. **legal_source** — ` v. ` case name, a reporter cite (`\d+ U.S. \d+`, `F.3d`, `F. Supp.`),
   named court, statute/USC.
2. **law_review** — `L. Rev.`, `Law Review`, `Law Journal`.
3. **thesis** — `dissertation`, `thesis`, `PhD diss`.
4. **dictionary_or_reference** — dictionary/encyclopedia (Merriam-Webster, Britannica,
   Wikipedia, Lexico, `/definition/`, `/dictionary/`).
5. **interview** — `interview`, `podcast`, `transcript of`, Q&A.
6. **journal_article** — a journal/periodical name **with** volume/issue or a
   `NN(N) (YYYY): p–p` pattern, or a journal DOI. **arXiv / SSRN preprints →
   journal_article** (scholarly works, not policy reports).
7. **book_chapter** — `in <Book>, ed.` / `chapter` with a container book + publisher.
8. **book** — a publisher (`Press`, `Publishing`), ISBN, no journal/volume signal.
9. **news_article** — a known news outlet/domain (Times, Post, CNN, Politico, Vox, Reuters,
   Guardian, NPR, BBC, Bloomberg, Atlantic, Slate, The Hill, …) or `news`.
10. **report** — think-tank / institute / foundation / gov agency / NGO as publisher
    (Brookings, Heritage, RAND, CRS, CSIS, UN, WHO, IMF), **working paper / white paper**.
    *(Preprints are journal_article per §6 — only institutional/policy PDFs are reports.)*
11. **web_page** — has a URL / blog / generic site and none of the above.
12. **unknown** — no discernible source signal (thin/incomplete cites).

## 8. Reject / bare-cite policy (pins the "Hall 17" question)

**Parse (status=parsed), never reject** — a bare/thin cite is a *complete parse of an
incomplete cite*, not a "low-confidence" one. There is **no `incomplete_citation` /
low-confidence marker**: missing fields are simply **absent** (sparse omission *is* the
"this field was missing" signal), and the deterministic interpreter detects/handles
missingness downstream (a well-understood problem it owns — see §Warnings).
- **Bare cite** — surname + year (± qualification), no title/url/source (e.g. `Hall 17`,
  `Silver '12`): author = surname, infer year, `source_type:"unknown"`, other fields omitted.
  *(These are real, convertible cites — never drop them.)*
- **URL-only**: set `url`; other fields omitted (deterministic layer notes "url only").

**Reject (status=reject) with reason:**
- `empty_or_placeholder` — `INSERT`, blank, `.`, `---`, `N/A`, `xx`, `TBD`.
- `cross_reference_only` — `Ibid`, `Id.`, `op. cit.` with no surrounding cite.
- `analytic_or_tag` — argument tags (`AT:`, `A2:`, `2AC`, `framework`, `perm`, `CP`, `DA`,
  `K`, …) and pure analysis/prose sentences / card body text with no citation header.
- `not_a_citation` — lyrics, headings, other non-source text.
- `too_malformed` — garbled beyond parse (use sparingly).

Reject objects carry `reject_reason` + `evidence` (first ~200 chars).

## 8b. Warnings design — model predicts structure; deterministic derives flags (APPROVED)

Your "the deterministic interpreter handles missingness" principle generalizes: most
`warnings` are **derivable** from the parsed structure + input text, so they don't belong
in the model's target at all. Splitting them:

| warning | derivable deterministically? | owner |
|---|---|---|
| `incomplete_citation` | yes — key fields absent | **deterministic** (removed from model, per §8) |
| `url_only` | yes — only `url` populated | **deterministic** |
| `source_type_ambiguous` | yes — `source_type == unknown` | **deterministic** |
| `et_al` | yes — `et al` in the input text | **deterministic** |
| `no_date` | yes — `ND`/`No Date` regex (w/ legal-district guard) | **deterministic** |
| `conflicting_dates` | yes — parsed year vs date-in-text mismatch | **deterministic** |
| `body_spillover` | **no** — needs the semantic "where does the cite end" read | **model** |

**Proposal:** the model predicts only `body_spillover` (+ `spillover_start_index/text`);
**all other warnings are computed by the deterministic Rust layer.** This shrinks the target
(fewer tokens, faster), removes fuzzy labels the model would otherwise imitate imperfectly,
and makes those flags 100% reliable. `no_date`/`et_al` stay *semantically* honored (the model
still sets `year:null` for no-date, still enumerates et-al authors) — only the *warning tag*
moves to deterministic.

**APPROVED.** §6's warnings vocab for the **model** collapses to `{body_spillover}`; the full
list survives only as the **deterministic layer's** output vocabulary. The target builder
strips all warnings except `body_spillover` from the Haiku labels; the Rust deterministic
layer recomputes the rest at inference.

## 9. Author rules

- Enumerate **every** named author in the body (incl. `et al.` expansions); add `et_al`
  warning when the head says "et al".
- `family` = **head surname is authoritative**; `given` = first/given name from the body
  (handle reversed order: `Morton, 13—Timothy` → family Morton, given Timothy).
- `literal` = the full name as written. `qualifications` = one blob string of the
  title/affiliation prose between the name and the title (no title/pub/date/url inside it).
- Never promote institutions (University, Foundation, Press, Journal…) to authors.

## 10. Dates

- Year shorthand: `00–30 → 2000–2030`, `31–99 → 1931–1999`, single digit `9 → 2009`,
  `2k → 2000`. Prefer the **publication** year; access/retrieved dates never overwrite it.
- `no_date: true` (year null) for `ND` / `N.D.` / `No Date`. **Never** treat legal districts
  (`N.D. Cal.`, `S.D.N.Y.`) as no-date.

## 11. Tokenization & lengths

- Tokenizer: stock Flan-T5 SentencePiece (`tokenizer.json`), **unchanged**. Loaded in Rust
  via the `tokenizers` crate.
- `max_source = 768`, `max_target = 640`. Rationale from the tokenized corpus: target
  p99 ≈ 537 tokens → 640 gives zero truncation with margin; 768 source captures more
  spillover boundaries. **Every training target MUST fit `max_target` with zero truncation**
  (gate in §13). Mega-cites are bounded by the §3 caps.

## 12. Labeling policy (Phase 1)

- **Primary labeler: Claude Haiku**, single pass, full corpus (~5,700 rows), labeling to
  this contract (conventions + §7 rubric + §8 policy in the prompt).
  - Validated: Haiku ≈ Sonnet on the structural fields (author count 0.996, given 0.827,
    year 1.00, url 0.99, title 0.98). Residual gap is source_type/reject *ambiguity*, fixed
    by §7/§8 + constrained enums — not a capacity gap.
- **No self-flag gate** — Haiku's `needs_review` was miscalibrated (recall 0.42, precision
  0.22); it over-flags yet misses most disagreements. Dropped.
- **No 2-Haiku consensus** (per decision) — marginal gain given the above. *(Reversible: a
  second Haiku pass + agreement filter if we later want it.)*
- **Eval set**: the held-out **250 natural-distribution gold stays Sonnet-labeled** and is
  the true quality benchmark (never trained on). The train/eval/**test** splits are all
  Haiku-labeled (the test split measures in-distribution fit; the Sonnet gold measures true
  quality). Derived `warnings` (§8b) are stripped when building targets and recomputed
  deterministically at inference — so Haiku may emit them freely; they don't reach the target.

## 13. Validation gates (all BEFORE training — the "be perfect" gate)

1. Every training target parses under the reconstruction rules (§4) and round-trips to
   valid JSON.
2. Every target's `source_type` / `reject_reason` / `warnings` ∈ the §6 enums.
3. Every target fits `max_target` tokens — **zero truncation**.
4. The constrained-decoding grammar (§6, built in Phase 2) **accepts exactly** the gold
   label set and rejects malformed strings — validated without a model in the loop.
5. Stratified train/eval/test split in final format; frozen.

Only when 1–5 pass do we spend a training credit.

---

## Open items before freeze
- **User sign-off on §7 (source_type rubric) + §8 (reject/bare-cite policy)** — the gate
  before the Phase-1 relabel spend.
- Confirm §5 override field list (esp. whether `year` override is worth the edge cases).
- Confirm `max_source`/`max_target` (768/640) — tune after the full relabel's length dist.

## Resolved
- **Candle feasibility (de-risk spike, done):** T5 generation ✅ well-supported;
  quantized T5 (GGUF q6k/q4k) ✅ well-supported — Small q6k ≈ 60 MB, Base q6k ≈ 260 MB;
  constrained decoding ✅ off-the-shelf via `llguidance`. Candle is a safe deployment target.
  One de-risk deferred to Phase 5: validate `tensor-tools quantize` on **our** checkpoint
  (low risk — we keep the stock tokenizer, no added tokens).
