# Sol brief #2 — drive citation silent errors to ZERO

You (sol, gpt-5.6-sol) have no context from my session. Here is everything. We collaborated before and you
were excellent — you added 10 recovery functions that killed a whole class of silents. Usage crashed you
mid-run last time; you're back. New, harder push now.

## The mission (unchanged, hard constraints)
- Flowstate ships a **FROZEN** Flan-T5 citation parser (CTranslate2 int8 CPU, vendored ct2rs). **NEVER retrain
  the model under any circumstances.** All fixes live in the deterministic Rust repair layer.
- A citation = messy debate "fullcite" string → structured JSON. We score ONLY the **author surname SET**
  (folded: lowercase, ascii-alnum only, particles dropped).
- **PRIMARY GOAL: ZERO silent author errors in production.** A "silent" = the harness ACCEPTED a cite (didn't
  flag it) but the extracted author-surname set ≠ the correct set. Silent is the cardinal sin (a wrong answer
  presented as correct). Over-flags are the acceptable lesser cost, but minimize them once silent is zero.

## What I did this session (the big wins are already in)
1. **Built a persistent decode cache + measurement harness.** The model decode is the only nondeterministic,
   expensive input; repair is a pure fn of (raw, source). So we decode each distinct cite ONCE and replay
   repair offline in ms. Key infra (all committed to disk, not git):
   - `crates/flowstate-citation/src/bin/citecache.rs` — cache-aware batched decoder (resumable).
   - `crates/flowstate-citation/src/bin/replay.rs` — offline repair+flag+score from dumped raws.
   - `datasets/citation_finetune/decode_cache.jsonl` — persistent input-keyed decode cache.
   - `datasets/citation_finetune/eval_labelled_all.jsonl` — the FULL labelled set: **6,105 distinct cites**
     (union of heldout 1416 + train 5210), {id, input, target}. id = sha1(norm_input)[:16].
   - `datasets/citation_finetune/labelled_raws.jsonl` — the 6105 decodes joined to gold, replay-ready.
2. **THE BIG FIND — the `family` CSL-alias bug.** The model emits CSL-JSON author keys `"family"`/`"given"`/
   `"literal"` instead of `"surname"`/`"name"` on ~24% of cites (it was trained on CSL-format labels).
   `reconstruct.rs`'s char-grammar is hard-keyed to `surname`, so it was **silently dropping the surname** on
   a quarter of all cites — invisible to the old heldout-only testing. Fix = 3-line alias at the top of
   `reconstruct::to_json`: `"family":`→`"surname":`, `"literal":`→`"name":`, `"given":`→`"name":`. This alone
   took labelled-set silent from **24.4% → ~5%**. (Do not touch this; it's correct and validated.)
3. **Gold-cleaned the silents with Sonnet** (11 parallel relabelers). RESULT: of the 520 apparent silents on
   the full 6105, **77% are TRAIN-GOLD LABEL ERRORS** (the model was right), not repair bugs. The clean gold
   for the silent set is at `datasets/citation_finetune/clean_gold/silent_relabels.jsonl` and
   `/tmp/claude-1000/clean_all.jsonl` ({id, surnames, type: person|org|none|ambiguous}).
4. Landed clean generalizable fixes (all ZERO-regression): roster over-fire fixes, `drop_non_name_authors`
   (degree tokens + safe degree-plurals phds/jds/mbas/llms), reverted a wrong particle change.

## Two CONVENTIONS the user confirmed (critical — get these right)
- **Particles DROP.** The surname is the core final word: `Van Rythoven`→`rythoven`, `De Angelis`→`angelis`,
  `de la Fuente`→`fuente`. The curated heldout gold is uniform on this. Gold entries that KEEP the particle
  are ERRORS; the model dropping it is CORRECT. (`normalize_authors` reduces multi-word surname to last word
  — keep that.)
- **Organizations → publication field, author BLANK.** When a cite is authored by an org/agency/wire-service
  with no named person (`Xinhua 15`, `HRW`, `USSD`, `GPO`, `EHP`), the correct author set is EMPTY (debaters
  have a fallback org-cite format). So the model returning `[]` is CORRECT. Do NOT recover orgs as authors.
  (This means ~33 of the "silents" are gold errors, not real.)

## CURRENT TRUE STATE (the target)
On the full 6,105-cite labelled set: **88.9% perfect, 8.5% apparent silent (520)**, but after gold-cleaning:
- **GENUINE repair-fixable silents: 118 (1.93%)** = **78 person-miss + 40 over-extract**.
- 77% of apparent silents are gold errors / org-as-blank / particle-keeping.
- unseen-430: 0 silent. 986 controls: 0 regression / 174 improvements. 14 tests + clippy green.

**All 118 genuine silents are enumerated in `/home/adam/Projects/flowstate/citation_genuine_silents.xlsx`**
with columns: raw input | raw model output | harness output | correct answer (Sonnet-cleaned) | category | id.
Read it (it's small). This is the exact remaining work.

## Why the last 118 are HARD (where I got stuck — I need your help)
Every clean gazetteer/frequency approach fails because the offending tokens are ALSO real surnames:
- **Given-name-as-surname** (`sergey`, `joanne`, `joseph`, `julian`, `emily`, `viktor`): the model made a
  lone given name from a byline into an author. But ALL of these are ALSO in the surname gazetteer, so
  "drop if ∈GIVEN and ∉SURNAMES" catches ~none.
- **Place/institution-as-surname** (`angeles`/`diego` from San Diego/Los Angeles, `stanford`, `mellon` from
  Carnegie Mellon, `vox`, `politico`): `Stanford`/`Mellon`/`Diego` are ALL real surnames too.
- **Near-dup misspellings** (`levitky`+`levitsky`, `sokolosky`+`sokolsky`, `hilsenwratch`+`hilsenrath`): the
  model emits BOTH the misspelled cite-tag spelling and the correct byline spelling as two authors. Both are
  grounded in the source; picking the right one needs byline-vs-tag POSITION logic. `drop_fabricated_near_dups`
  in snap.rs misses them (threshold too strict + both grounded).
- **Surname-first `SURNAME, Given`** (`VARGAS, João H. Costa`→ model took `costa`, correct `vargas`).
- **Wrong-person / interviewer** (`Porter 14 – Interview between Chris Matthews and Michael Porter` → model
  took `matthews`; correct `porter`).
- **Byline omissions** (model got 3 of 4 authors in a big list).
- **Model typos snap should fix from source** (`Behrant`→ source has `BEHRENT`; `strawpublished`← snap glued
  `Straw` to hyphenated source token `Straw-Published`).

The theme: these need CONTEXT-AWARE rules (byline structure, position relative to the year/tag, "San/Los"
place prefixes, "@ Affiliation", "Interview between X and Y", the fuller-name spelling) — not blunt token
drops — AND they must be ZERO-regression against the 986 controls + 430 unseen + not regress the 6105.

## THE ASK
Help me eliminate **ALL 118** genuine silents (person-miss + over-extract) with clean, generalizing,
zero-regression deterministic rules. Goal is literally zero silent in prod. Group the 118 into fixable
classes, design the cleverest rule per class, and implement + validate. If a case is genuinely unfixable
without regression (a true ambiguity or a source typo the source itself gets wrong), say so explicitly and
justify — don't fabricate a fix that trades silent for over-flag or regresses real names.

## How to work / measure (offline replay — instant, no model cost)
```
cd /home/adam/Projects/flowstate
cargo build --release -p flowstate-citation --bin replay
# full 6105 vs the (noisy) train gold — headline silent number:
./target/release/replay datasets/citation_finetune/labelled_raws.jsonl \
    datasets/citation_finetune/eval_labelled_all.jsonl --out /tmp/scored.jsonl
# ZERO-REGRESSION GUARDRAILS (must never regress):
./target/release/replay scratchpad_raws_all.jsonl scratchpad_heldout.jsonl      # 986 controls
./target/release/replay scratchpad_raws_unseen.jsonl scratchpad_heldout.jsonl   # 430 unseen
```
- To judge REAL silent (excluding gold noise), re-score against the clean gold:
  `python3 tools/citation_finetune/clean_rescore.py /tmp/scored.jsonl /tmp/claude-1000/clean_all.jsonl`
  (and remember: type==org means author should be BLANK; particle-keeping in a clean label = gold error).
- **CONFOUND WARNING:** int8 greedy decode is NOT bit-reproducible run-to-run. NEVER diff a fresh decode vs an
  old scored file — always replay over the SAME cached raws (`labelled_raws.jsonl` / `scratchpad_raws_*`).
- The repair pipeline is `repair()` in `crates/flowstate-citation/src/lib.rs` (ordered list of
  normalize/snap passes). Author-field logic lives in `normalize.rs` (`reconcile_surname_name_inner`,
  `clean_surname`, `drop_non_name_authors`) and `snap.rs` (grounding, recovery fns, `drop_fabricated_near_dups`).
  Field parsing/CSL-alias is `reconstruct.rs`.
- Run tests + clippy before declaring done: `cargo test --release -p flowstate-citation` and
  `cargo clippy --release -p flowstate-citation` (must be 0 errors).

## Guardrails
- FROZEN model — deterministic repair only.
- NO memorization: no hardcoded surnames/ids/title strings keyed to the eval set. Rules must generalize.
- ZERO regression on the 986 controls + 430 unseen. Validate EVERY change by replaying both.
- Don't trade silent for over-flag or regress real names to "fix" a case. A principled decline beats a hack.
- Nothing is committed; leave it uncommitted for the user to review.

Read `citation_genuine_silents.xlsx` first, then the clean gold, then go. Report the class breakdown, your
per-class rule design, and the before/after silent + regression numbers. Let's get to zero.
