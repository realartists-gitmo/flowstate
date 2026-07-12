# Citation model — latency & deployment protocols

**Mandate: equity / accessibility.** Latency must be usable for *every* end-user, on
whatever hardware they have — an old laptop, a low-power device, no GPU. We do not gate
this tool behind fast/expensive machines, and we do not build "fast paths" that assume some
inputs are easier (there is no such thing as a clean/common cite — that assumption is how
dirt accumulates). Every protocol below applies **uniformly to 100% of inputs** and is
chosen so the slowest realistic end-user still gets acceptable latency.

Physics: `latency = (output tokens) × (per-step cost)`, decoded autoregressively. We attack
both factors, uniformly.

## Runtime decision: CTranslate2 (CPU) — bake-off decided

We abandoned **candle** (pure Rust, but weak quantized kernels and no Intel/AMD iGPU path).
A same-box bake-off (Small, dense, 50 gold rows, i7-13620H) chose **CTranslate2 on CPU**:

| runtime | device | median ms/cite | correctness | note |
|---|---|---|---|---|
| **CTranslate2** | **CPU** | **~817** | full | ✅ chosen — ~2.3× candle, same accuracy |
| OpenVINO | iGPU fp32 | 1373 | full | slower than CT2-CPU |
| OpenVINO | CPU | 1429 | full | |
| ONNX Runtime | CPU | 1785 | full | weak seq2seq CPU |
| candle (fp16+MKL) | CPU | 1900 | full | old baseline |
| OpenVINO | iGPU fp16 | 590 | **BROKEN** | T5 fp16 instability → garbage |
| llama.cpp | — | — | — | T5 CLI crashes; needs custom libllama harness |

**iGPU ruled out for T5:** fp16 (where iGPUs win) corrupts T5 (unscaled attention overflow);
fp32 on this weak Intel iGPU is slower than CT2-CPU. This is *good* for equity — CT2-CPU is
universal (every machine has a CPU), no GPU driver / precision fragility, same path for all.
Tradeoff accepted: CT2 is C++ (weak Rust bindings) → we give up pure-Rust.
Door left open: CT2 **int8** typically ~300–400 ms if the no-quant rule is ever relaxed.

## Accuracy pipeline: precision cascade + deterministic repair + QA gate

Base model, int8 for the first candidate and float32 for mandatory verification. Every output is
run through deterministic repairs, strict schema validation, and failure-mode checks. Success
requires two valid candidates to agree on status and author identities; disagreement is evidence
of exactly the omission/attribution uncertainty that grounding alone cannot observe.

### The ladder (each stage: decode → parse+repair → schema/checks; snap runs after every decode)

    int8 greedy + f32 greedy
      ├─ both valid and status/author identities agree ⇒ accept the earlier int8 candidate
      ├─ both valid but disagree ⇒ HUMAN REVIEW (`decode_*_disagreement`)
      └─ either invalid ⇒ beam k=8, then up to 5 int8 samples, seeking a second agreeing decode

    any input/output length ceiling ⇒ HUMAN REVIEW (`decode_*_length_limit`)
    no two valid agreeing decodes ⇒ HUMAN REVIEW (`decode_no_consensus`)
    └─ retain the best-grounded candidate seen, not the last

The deployed checkpoint and tokenizer use 3072-token input/output ceilings. A normal citation
still stops at EOS; the larger output bound does not force padding or extra decode steps. Returning
EOS lets the backend distinguish normal completion from a capped prefix before JSON repair.

### Failure-mode checks (deterministic, no reference) — what triggers repair/escalation

- `invalid_json` — output won't parse even after the repairs below.
- `surname_ungrounded` / `name_ungrounded` — author identity words are not grounded in the source
  (fuzzy: difflib ratio ≥ 0.85 or containment, diacritic-folded; initials are skipped).
- `schema_*` — syntactic JSON that violates the sparse object contract, including incomplete
  author objects, wrong field types/enums, or unknown fields.
- `decode_*_length_limit`, `decode_*_disagreement`, `decode_no_consensus` — model-completeness
  and uncertainty failures owned by the controller rather than the deterministic checker.
- **DROPPED** `under_enumeration` and `title_ungrounded`: measured 1/7 and 0/1 precision with
  zero successful recoveries — they only manufactured false human-review by counting
  capitalized words in quals/institutions as phantom authors. Re-add only with a precise impl.

### Four deterministic repairs (model-free; the Rust Phase-2 reconstructor owns all of them)

1. **Quote-repair** — T5 emits titles with unescaped inner quotes (source curly quotes
   `“ ”` collapse to `"`, and the escaping `\` is a rare token the model drops), which
   terminate the JSON string early. Re-escape every *content* quote using the fixed key
   grammar: a `"` only closes a value when followed by `]`, end, or `,"<known-key>":`.
2. **Bracket-balance** — drop spurious closing brackets / append missing ones, respecting
   string state (fixes `…School"]]` — one `]` too many).
3. **Snap-to-source** — the model's job is extraction, so a mangled name is a corruption of a
   token present *verbatim* in the source. Snap each flagged name field to the nearest source
   span, **positionally anchored** on the author's sibling field: fuzzy-locate the `name` in
   the source, then the surname is the adjacent token (the anchor lookup is itself fuzzy, so a
   corrupted anchor like `Toto` still finds `Tomo`). Similarity is **Jaro-Winkler** (Rust:
   `strsim`/`rapidfuzz`) — purpose-built for name typos (`Javad`/`Jaded`≈0.86, `Tomo`/`Toto`≈
   0.93 where difflib scored only ~0.6). Adjacency candidates use a looser 0.72 gate (position
   justifies it) + subsequence match; global candidates need 0.80. Fixes character typos
   (`Wink`→`Wick`, `Solee`→`Soule`) and — critically — restores characters T5's vocab **cannot
   emit** (`ari`→`Šarić`). Applied only to flagged outputs (zero blast radius on the ~96% that
   pass; **verified 0 regressions / 5 recoveries** over the 250 gold set). Reference: `snap.py`.
4. **normalize_authors** — deterministic author cleanups: strip credentials/year fragments and
   generational suffixes, reconcile `surname` with the full `name`, and deduplicate decode
   repetitions without collapsing real same-surname coauthors. Applied to every parsed output.

### Best-grounded handoff (spec — implement in the Rust port)

When every tier fails and a cite must go to a human, hand over the **best-grounded candidate
seen across all decode attempts**, not the last one. Escalation can *degrade* the text
(observed: int8 `Tomo ari` → beam `Toto ari`), and snap works better on the cleaner earlier
output — so the last decode is often the worst thing to show a human. Selection metric:
maximize grounded-author count (equivalently, minimize ungrounded name-token count under the
same fuzzy/diacritic rules the checks use); tie-break to the earliest (cheapest) tier. Track a
running best as each tier's post-snap candidate is produced; emit it if the ladder exhausts.

### Known residual

Agreement between decodes is an uncertainty detector, not a mathematical proof: correlated tiers
can still omit the same author. Inputs beyond 3072 tokens fail loudly. Within the window, the
remaining correlated-omission rate must be measured against held-out labels and retained as an
explicit quality metric; no capitalization-based name counter is treated as a correctness oracle.

### Historical repair results (250-cite gold, before mandatory decode consensus)

| stage | human review |
|---|---|
| 3-tier precision cascade (start) | 14.8% |
| + beam / best-of-N | 10.8% |
| + tightened checks (drop dead checks, diacritic-norm, fuzzy) | 3.2% |
| + reconstructor repairs (quote + bracket → **100% valid JSON**) | 2.0% |
| + snap-to-source (after every decode) | ~1.2% |
| + Jaro-Winkler snap + fuzzy anchor + normalize_authors | **0.0%** |

The old controller stopped after the first valid int8 result (96% of this set), which made these
numbers useful for measuring deterministic repair but did not protect against a clean-looking
author subset. Production now always obtains a second valid decode and requires status/author
agreement. Beam and best-of-N remain insurance for cases where one of the first two candidates is
invalid; they never overrule a valid disagreement.

## Adopted (all of these — this is the standing plan)

1. **Platform-optimal BLAS, auto-detected — but it only helps the DENSE path.** Link the
   fastest matmul backend per hardware: **MKL** (x86), **Accelerate** (Apple), **CUDA**
   (GPU), generic gemm floor. **Measured caveat:** candle's *quantized* `QMatMul` uses its
   own ggml-style kernels, NOT BLAS — so MKL barely helps quantized inference (only the dense
   per-token `lm_head`). MKL fully accelerates a **dense (f16/f32)** model. Also: MKL threads
   contend with candle's rayon — pin `MKL_NUM_THREADS=1` for the quantized path, let MKL use
   all threads for the dense path. Measured on i7-13620H, small model:
   naive quantized 5.8 s/cite → quantized+MKL(1) 3.5 s → **dense-fp16+MKL 1.9 s/cite**.
   Real speedup was ~3× (not the 10× I'd hoped) because most layers are quantized on the
   quantized path — hence protocol 3.

2. **Persistent, warm-loaded model + full threading.** Load the model once, keep it resident
   in a long-lived process, use all cores. No per-request load tax; cold starts amortized.

3. **Fast-first: ship the smallest model that clears the bar, DENSE (fp16), not quantized.**
   Priority is latency ≫ size (per Adam). On candle, dense-fp16+MKL beat quantized by ~2×
   (see #1), so quantization — a *size* play — is dropped for the default deployment.
   Model-size (Small vs Base) is still the ~2× lever and is the real fast/quality tradeoff.
   **Quantized (q4k) is the fallback only** for a genuinely memory-constrained target that
   accepts the slowdown. Footnote: candle's weak quantized CPU kernels are why quantized is
   slower here; a ggml/llama.cpp-class runtime would make quantized both fast *and* small —
   but we chose candle (pure Rust), so dense it is.

5. **Compact serialization.** Short field tags in the emitted format (every saved token is a
   saved decode step, uniformly). *Format change → must be fixed before the production
   retrain; define the short-key mapping and update `target_schema.json` + the Rust
   reconstructor/grammar to match.*

6. **Batching for bulk.** For any end-user processing many cites at once, batch multiple
   cites per forward pass — large throughput multiplier (doesn't lower single-cite latency,
   raises total).

## Considered and REJECTED

- **(4) Reduced target** — dropping url/doi/pages/volume/issue/dates from the model's output
  and regex-recovering them from the input. Rejected: in messy debate cites these fields are
  **not** reliably regex-recoverable (mangled URLs, line-split DOIs, varied page/volume
  formats). The model stays the source of truth for every field.
- **Span-pointer output** (emit `[start,end]` offsets instead of verbatim text). Rejected for
  T5: seq2seq models can't emit exact character offsets reliably — trades latency for
  accuracy. (Would suit an encoder-only tagger, but we also need generation/classification.)
- **Per-cite fast-path routing** ("this one looks clean → skip the model"). Rejected on
  principle: assuming clean inputs exist is the failure mode that lets label/parse dirt
  accumulate. Every cite gets the full model.

## Implementation gotcha (Candle) — MUST fix or output is garbage
Flan-T5's `config.json` says `tie_word_embeddings: true`, but our fine-tuned model is
**untied** (a separately-trained `lm_head.weight`). candle honors the config flag and will
project outputs through the shared embedding → total garbage (repeats one token). **Set
`tie_word_embeddings = false`** (patch the shipped config) so candle loads the real
`lm_head.weight`. Also: quantize from **f32** safetensors (cast f16→f32 first), and T5's
vocab lacks `{`/`}` so the decoder emits brace-free JSON that the Rust reconstructor rebuilds.

## Phase-2 (Rust) port — the deterministic layer

The four repairs + the failure-mode checks + the cascade controller are all model-free and
**must be ported to Rust** as the reconstructor/QA layer around the CT2 (C++) inference call.
Python reference impls live in the bake-off: `repair.py` (`escape_content`, `balance_brackets`,
`to_json2`), `snap.py` (`snap_authors`, `normalize_authors`, `_jw` Jaro-Winkler), `cascade8.py`
(checks + `resolve()` + ladder). Port notes: the key grammar is fixed (see `target_schema.json`
key set); snap needs diacritic-fold (Unicode NFKD + strip combining marks) and **Jaro-Winkler**
(`strsim`/`rapidfuzz` crate — not plain Levenshtein/ratio); adjacency snaps gate at 0.72, global
at 0.80; keep the best-grounded candidate across tiers to hand the human, not the last decode.

### Two model-side fixes to bundle into the production retrain (not inference repairs)
- **Preserve source curly quotes** `“ ”` in title/quals verbatim instead of straight `"` — they
  can't collide with the JSON delimiter, eliminating the quote-repair need at the root.
- Bundle with **Protocol 5 compact keys** (below) — one retrain, one frozen-format change.

## Cross-refs
- Output format: `output_contract.md` (§5 hybrid is now *validate/supplement*, not override).
- Accuracy pipeline + three repairs: this file, "Accuracy pipeline" section above.
- Protocol 5 (compact keys) is a frozen-format change — bundle it into the final production
  retrain, not a separate one (with the curly-quote target fix above).
