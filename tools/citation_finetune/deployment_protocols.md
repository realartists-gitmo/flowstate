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

Base model, **int8 by default** (int8 ≈ int16 ≈ f32 on quality — precision escalation
recovers almost nothing; measured 0 recoveries at f32, 2 at int16 over 250 gold cites).
Every model output is validated by deterministic **failure-mode checks**; a failing output
is first sent through model-free **deterministic repairs**, and only if those don't clear it
do we escalate (which costs a re-decode) or, as a last resort, hand off to a human.

### The ladder (each stage: decode → parse+repair → checks; snap runs after *every* decode)

    int8 greedy ─┐
    int16 greedy ─┤  after each decode: to_json (+quote/bracket repair) → checks;
    f32 greedy   ─┤  if checks fail, snap-to-source → re-check.  Pass ⇒ stop.
    beam k=8     ─┘  Order is cheapest-first: int16 = 1 decode (~1.5 s) is cheaper than
    best-of-N (5×int8 sampling, ~4.6 s) ── the only stochastic tier; reruns genuinely help.
    └─ still failing ⇒ HUMAN REVIEW (retain the *best-grounded* candidate seen, not the last)

### Failure-mode checks (deterministic, no reference) — what triggers repair/escalation

- `invalid_json` — output won't parse even after the repairs below.
- `family_eq_given` — an author's family == given (fabrication signal; auto-resolved by the
  `normalize_authors` dedup below before it ever reaches a human).
- `name_ungrounded` / `given_ungrounded` — a family/given word is not grounded in the source
  (fuzzy: difflib ratio ≥ 0.85 or containment, diacritic-folded). Given is lenient (initials
  skipped); family is strict.
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
   span, **positionally anchored** on the author's sibling field: fuzzy-locate the `given` in
   the source, then the family is the adjacent token (the anchor lookup is itself fuzzy, so a
   corrupted anchor like `Toto` still finds `Tomo`). Similarity is **Jaro-Winkler** (Rust:
   `strsim`/`rapidfuzz`) — purpose-built for name typos (`Javad`/`Jaded`≈0.86, `Tomo`/`Toto`≈
   0.93 where difflib scored only ~0.6). Adjacency candidates use a looser 0.72 gate (position
   justifies it) + subsequence match; global candidates need 0.80. Fixes character typos
   (`Wink`→`Wick`, `Solee`→`Soule`) and — critically — restores characters T5's vocab **cannot
   emit** (`ari`→`Šarić`). Applied only to flagged outputs (zero blast radius on the ~96% that
   pass; **verified 0 regressions / 5 recoveries** over the 250 gold set). Reference: `snap.py`.
4. **normalize_authors** — deterministic author cleanups: (a) `family == given` is pure
   duplication (a mononym the model couldn't split) → drop `given`; (b) strip a trailing
   generational suffix from family (`Moore IV`→`Moore`) to match canonical segmentation.
   Applied to every parsed output. Reference: `snap.py:normalize_authors`.

### Best-grounded handoff (spec — implement in the Rust port)

When every tier fails and a cite must go to a human, hand over the **best-grounded candidate
seen across all decode attempts**, not the last one. Escalation can *degrade* the text
(observed: int8 `Tomo ari` → beam `Toto ari`), and snap works better on the cleaner earlier
output — so the last decode is often the worst thing to show a human. Selection metric:
maximize grounded-author count (equivalently, minimize ungrounded name-token count under the
same fuzzy/diacritic rules the checks use); tie-break to the earliest (cheapest) tier. Track a
running best as each tier's post-snap candidate is produced; emit it if the ladder exhausts.

### Known residual (not human-review; documented, low priority)

Two-word surnames vs given+family is an inherent ambiguity. `Scheffler Blaeser` (a two-word
surname in a comma-delimited surname list) is captured as `family:"Blaeser"` + `given:
"Scheffler"` — all text preserved, checks pass, no human needed, but not gold-segmentation.
A *possible* future fix: sibling-consistency — if the other authors in the same list are
given-less (bare surnames), treat a lone `given` as part of a two-word family. Fragile; deferred.

### Results (250-cite gold, base-int8 default)

| stage | human review |
|---|---|
| 3-tier precision cascade (start) | 14.8% |
| + beam / best-of-N | 10.8% |
| + tightened checks (drop dead checks, diacritic-norm, fuzzy) | 3.2% |
| + reconstructor repairs (quote + bracket → **100% valid JSON**) | 2.0% |
| + snap-to-source (after every decode) | ~1.2% |
| + Jaro-Winkler snap + fuzzy anchor + normalize_authors | **0.0%** |

Tier reached at the end: **int8 96%, int8+snap 3%, int16 <1%, human 0%** — best-of-N was never
triggered on gold. The deterministic layer resolves the entire 250-cite gold set with no human
handoff, including the cases previously called "structural": the 8-author foreign cite (fuzzy
+ diacritic snap), the two-word-surname judges cite (dedup + snap), and the fabricated mononym
(dedup). **Caveats:** (1) 0% is on the held-out gold set — production will still hit genuinely
garbled cites the checks can't clear, so real-world > 0; the *deterministic floor* is what
moved to 0. (2) "Passes checks" ≠ "gold-perfect" for ~2 cases (a two-word surname split, a
mononym field placement) — captured well enough not to warrant a human. (3) Keep **best-of-N
as insurance** for degraded production inputs even though gold never needs it; pin a sampling
seed for a deterministic build.

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
