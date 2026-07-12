# Brief for `sol` — citation repair layer: help us drive silent failures to zero

You (`sol`, gpt-5.6 max) are being asked to find repairs we're missing. You have **max thinking** — please use it. We (a Claude agent) have spent a long session on this and cut silent failures by 71% with zero regressions, but the last ~50 feel hard. We are probably missing something. Your job: read this, reproduce our measurement, and find generalizing fixes for the residual — OR tell us convincingly which residual cases are genuinely unfixable (gold-label errors / true ambiguities) so we can stop.

Everything below is context you don't have. Read it fully before acting.

---

## 1. The mission and the hard constraints

**Product:** `crates/flowstate-citation` — a Rust crate that parses messy debate "fullcite" strings into structured citation JSON. A fine-tuned **Flan-T5-small** (CTranslate2 int8, native CPU via vendored `ct2rs`) does the extraction; a **pure-Rust deterministic layer** repairs the model's output.

**HARD CONSTRAINT — the model is FROZEN. We will never retrain it.** So every fixable failure must be carried by the deterministic repair layer. Do not propose retraining, fine-tuning, prompt changes, or decoding-parameter changes. The model's raw output is a fixed input.

**Primary metric: ZERO silent author failures.** A "silent" = the harness *accepted* (passed, not flagged) a citation whose **author-surname set** does not equal gold. Scoring is on the folded set of author `surname` fields only (not name, not order, not other fields). Secondary metric: minimize **over-flags** (flagging a citation that was actually correct — the loud/annoying cost). A flagged-wrong case is NOT a silent; flagging is an acceptable escape hatch, but over-flagging has a cost.

**Rules of engagement (from the human):** rules must be *clever and generalize*; **no genuine regressions** (do not fix one case by breaking another); minimize over-flags. "Excluding gold-label errors and ambiguities" is allowed — if a residual is genuinely a bad gold label, say so with evidence.

---

## 2. Architecture / data flow

Raw decode is brace-free (T5 can't emit `{`/`}` reliably, so braces render as the literal token `<unk>`). Pipeline (`src/lib.rs::repair`):

```
reconstruct::to_json(raw)      // parse brace-free text -> serde_json::Value
  -> normalize::normalize_authors(o, source)   // clean surname/name, reconcile, dedup, strip_affiliation_markers
  -> snap::snap_surnames / snap_surname_span / snap_cite_tag / snap_names / ground_names  // fuzzy-correct to source tokens
  -> snap::recover_empty_author / recover_key_coauthors / recover_byline_coauthors        // add dropped authors
  -> snap::drop_fabricated_near_dups / snap_title
  -> normalize::strip_superscript_markers(o, source)
  -> normalize::drop_phantom_authors / normalize::normalize_authors(o, source)  // second reconcile pass
```

The **flagger** (`src/flag.rs::review_reason` + `review_run`): after repair, decide ship-vs-flag using source-grounding signals (ungrounded surname, all-caps institution, et-al undercount, role-person, named-chair) PLUS **input-perturbation decorrelation** — it re-decodes a meaning-preserving perturbation of the input and flags if the author-surname set disagrees. Perturbation catches shaky omissions/over-extractions. Production path = `process_with_mode(raw, source, FlagMode::Passthrough)` (repair only) then `review_reason`; `review_run` adds the perturbation decode.

Key source files (all under `crates/flowstate-citation/src/`):
- `lib.rs` — `repair`, `process_with_mode`, `process_raw`. NOTE: `process_raw` has a *fallback* stage (`snap_authors`) that can UNDO marker strips — the review path does NOT use it, so measure via `process_with_mode(Passthrough)`.
- `normalize.rs` — `normalize_authors`, `reconcile_surname_name_inner` (surname=last-token logic), `strip_superscript_markers`, `strip_affiliation_markers`, `clean_surname`/`clean_name`, `drop_phantom_authors`.
- `snap.rs` — `words()` (source tokenizer; splits on whitespace + `[]{},`), `snap_word`/`snap_surnames`/`snap_names`/`snap_cite_tag`, `ground_names`, `segment_person` (extracts First-Last from a byline segment; has a big STOP list), `recover_byline_coauthors`/`recover_key_coauthors`/`recover_empty_author`, `opens_quoted_title`.
- `flag.rs` — `review_reason`, `review_run`, `review_batch`, `has_other_author_name`.
- `gazetteer.rs` — `GIVEN_NAMES`, `SURNAMES` (frequency-filtered multinational name lists), `COMMON_WORDS`, `is_common_word`. Data in `data/*.txt`. These are Feist-safe fact lists; frequency was the build-time filter (so `Katharine` with the -a- spelling is filtered out but `Katherine` is in).

---

## 3. How to reproduce our measurement (do this first)

We built an **offline replay harness** so you can iterate in milliseconds without paying for the model. The model decode is captured once; repair is a pure function of `(raw, source)`.

**Data files (in repo root `/home/adam/Projects/flowstate/`):**
- `scratchpad_heldout.jsonl` — 1416 gold cites. Each line: `{"id", "input", "target"}` where `target` is a JSON *string* of the gold citation. `input` starts with `parse citation: `.
- `scratchpad_raws_all.jsonl` — 986 captured decodes (all 176 original silents + 810 regression controls, incl. 200 hardball-perfects). Each line: `{"id","input","raw","orig_issue","praw","praw_issue","has_perturb"}`. `raw` = the model's brace-free decode.
- `scratchpad_new_all.jsonl` — current scored output (with our fixes). Each line has `passed`, `correct`, `model_authors`, `gold_authors`, `final_obj` (the full repaired citation), `model_surnames`, `gold_surnames`.
- `scratchpad_base_all.jsonl` — baseline scored output (pre-fix code). Used for regression diffing.
- `/tmp/claude-1000/replay_base` — the compiled BASELINE replay binary (pre-fix code). **Critical:** int8 greedy decode is NOT bit-reproducible run-to-run, so you must diff new-replay vs baseline-replay on the SAME raws — never against a differently-decoded file.

**The bins** (build with `cargo build --release -p flowstate-citation --bin replay`):
- `src/bin/dumpraw.rs` — decodes cites once (per-cite; the batched path bloats to 6 GB and OOMs). Usage: `dumpraw <model_dir> <labelled.jsonl> --threads N --out raws.jsonl`. Model dir: `datasets/citation_finetune/models/ct2_small_fp32`.
- `src/bin/replay.rs` — offline repair+flag+score from dumped raws. Usage: `replay <raws.jsonl> <heldout.jsonl> --out scored.jsonl`. Prints perfect/SILENT/over-flag counts.

**Reproduce current numbers:**
```bash
cd /home/adam/Projects/flowstate
cargo build --release -p flowstate-citation --bin replay
./target/release/replay scratchpad_raws_all.jsonl scratchpad_heldout.jsonl --out scratchpad_new_all.jsonl | grep -E "SILENT|precision"
# baseline (pre-fix) on the SAME raws:
/tmp/claude-1000/replay_base scratchpad_raws_all.jsonl scratchpad_heldout.jsonl --out scratchpad_base_all.jsonl | grep SILENT
# per-id fixes vs regressions:
python3 scratchpad_diff.py scratchpad_base_all.jsonl scratchpad_new_all.jsonl
# silent taxonomy + per-case dump:
python3 scratchpad_silents.py scratchpad_new_all.jsonl | head -80
```
Expected now: baseline 174 silent → new **50 silent**, 125 fixes, **0 regressions**, over-flag 20→19.

**IRON RULE for any change you propose:** rebuild `replay`, re-run on `scratchpad_raws_all.jsonl`, and diff vs `/tmp/claude-1000/replay_base`. A fix is only real if FIXES go up and REGRESSIONS stay 0. The 810 controls (esp. the 200 hardball-perfects, ids `9000xxxx` that were already correct) are the regression tripwire — the academic `Surname INITIALS` and Hispanic `-a` bylines only break there.

Do NOT rebuild the baseline binary — it's already the true pre-fix reference. If you change code, only rebuild the normal `replay`.

If you need more decoded cites (e.g. to widen controls), run `dumpraw` on a subset jsonl as a **detached `systemd --user` service** — the machine is shared and heavily CPU-contended (load 20-36), foreground jobs get starved. Example pattern is in `scratchpad_measure.sh`.

---

## 4. The 6 fixes already landed (don't redo these)

All net-validated (125 fixes / 0 regressions total). Details in `~/.claude/projects/-home-adam-Projects-flowstate/memory/citation-repair-layer-fixes.md`.

1. **surname = last significant name token** (`reconcile_surname_name_inner`). Gold surname is uniformly the LAST token of the full name (verified 4532/4535). Strips a trailing initials-block (`Schwartz SLD`→Schwartz, `Du LW`→Du) only when the token before it is a Titlecase surname carrying a lowercase letter (keeps all-caps `RICH`, keeps `LeBlanc`). Guard: don't take the last token if it's followed by a bibliographic marker in source (`Symploke, Volume 23`). Fixed the big `Rob Lewers Davies`→Davies cluster.
2. **superscript affiliation-marker strip** (`strip_superscript_markers`) for bracketed `[Author^b,a, …]` cites: strip a trailing key `{a-f}` when the marked form appears as `<tok>,` in source. Gate = ≥2 double-keys `,x,`, OR ≥1 + bracket, OR STRUCTURAL (bracketed, ≥3 name-tokens ending in `{a-d}` outnumbering `{e-z}` endings, ≥2 distinct keys). Plus garbled-marker recovery (`name:"Bertrand a"` → surname is the source word after the given, `Bertrand Raméb`→Ramé). Enabler: `words()` splits on comma so `Kovacicb,a` tokenizes as `Kovacicb`+`a`.
3. **strip_affiliation_markers start-at-`a` anchor**: the old consecutive-alphabet-endings detector truncated real surnames coincidentally ending g,h,i (`Leidig`/`Krulwich`/`Harsanyi`); now requires the run to start at key `a`/`b`.
4. **over-extract guards** (`recover_byline_coauthors`): drop a recovered segment that opens the quoted title (`"Caring Capitalism"`); STOP-word additions in `segment_person` (graduate/student/universitat/watch/indian/nation/institution) kill `UC San Diego`→Diego, `Gene Watch UK`→UK.
5/6. **recover_empty_author**: when model returns zero authors but the cite-key surname reappears as a byline person, add it — gated on given ∈ GIVEN_NAMES or middle-initials present, and rejecting all-caps-acronym orgs (`DSCA`→Agency, `OTA`→Assessment were fabrications we killed).

---

## 5. The residual 50 silents — WHERE WE'RE STUCK

Full per-case data: `python3 scratchpad_silents.py scratchpad_new_all.jsonl`, and human-readable 3-column sheet `citation_silent_failures.xlsx` (raw input / raw model output / final harness output). Taxonomy: **17 omission, 24 corruption, 9 over-extract.** Our difficulty analysis (challenge it!):

**A. Omissions (17)** — model dropped coauthor(s). Two sub-kinds:
- *Big multi-drops* (e.g. `90001278` dropped 21 of ~40 authors; `90001267` dropped 9; `90001243` dropped 7). These are long `;`-delimited author lists with interspersed affiliations. Our `recover_byline_coauthors` needs a contiguous run of ≥2 person-segments containing an already-extracted anchor; interspersed affiliations break the run. We designed a "global recovery" (if ≥3 anchors anywhere in the byline, recover every clean person-segment) but SHELVED it: (a) a cite only flips to correct if we recover ALL drops, and many drops are mononyms (`Carafano`, `Idso` — single-token, unreachable by First-Last segmenting) or rare-given names not in the gazetteer; (b) aggressive global recovery risks over-extracting bio-embedded names. **Is there a safe way?** The delimiter structure is homogeneous (`;`-separated), and the model already proved it's an author list by extracting 15-20 of them. Maybe a delimiter-homogeneity signal we haven't exploited.
- *Single-drops* (`Moon`, `Pinch`, `Prieto`, `Bulman-Pozen`, `Lowther`, `Larison`, `Dore`, `Webb`, `Marshall`, `Robinson`, `Maciel`, `Medina`). More tractable but each needs to recover exactly the one missing surname. `Moon` (id 139716): title fuses into byline with no delimiter (`…Korea Relations Katharine H.S. Moon`), so a walk-back grabs `Relations`. `Bulman-Pozen`: cite key only, no byline to confirm the person.

**B. Corruption / disjoint (24)** — wrong surname. Sub-kinds:
- *2-author marker cites below our ≥3 structural threshold* (`90004635` Jenkinsd, `90004701` Heinerb, `90004695` Goa). The marker gate needs ≥3 marked tokens; 2-author bracketed cites slip. Lowering to ≥2 risks the `Garcia`/`Lloyd` regression (real bracketed bylines whose surnames end a-d). `Goa`: surname `Go` is only 2 chars so stem `Go` fails our ≥3-char stem guard.
- *Model garbles / source typos* (`Arkinson` — the SOURCE cite-key literally misspells Atkinson though the body has "Rob Atkinson"; `Nadia C` — gold surname is `C`, model picks `Nadia`; `Kwok-chuen` mononym). Some of these may be gold-order oddities.
- *Big-list single-corruptions* — a 30-author cite where one surname is wrong (`90001217` Sebastien vs Svetlič+Belkin; `90001225` Rice vs Bolton) — usually the model grabbed a bio name.

**C. Over-extract (9)** — model added a non-author. Sub-kinds:
- *Model reads a publication/org as the SOLE author*, gold empty (`Welle`←Deutsche Welle, `Today`←Al Khaleej Today, `Lumen`←Lumen Learning, `Saenz:`/`Porous:` — inputs that are literally just a name+colon). These need the FLAGGER (loud) or a "sole mononym author that is a publication" heuristic — repair can't know it's an org. Risk: over-flagging real mononym authors.
- *Recovery/model fabrications still slipping*: `Capitalism`←`"Caring" Capitalism` (title where only "Caring" is quoted, so our quote-guard misses it); `Water`←`UNESCO Water`; `Sanchez` (model duplicated an author); `Yaroslav` (cite key `Slabykh and Yaroslav` uses a given name as a key surname, recovered via `recover_key_coauthors`).

---

## 6. What we TRIED and REJECTED (don't repeat unless you have a genuinely different signal)

- **Global-frequency name-order flips** (given-vs-surname frequency to fix `Bojian Liu`-type order): 8/9 on curated pairs but **106 held-out regressions** — real rare surnames are unknown to the dataset and got swapped to their given-name partner; many given names also rank as surnames somewhere. Reverted. Do not retry global-frequency name-order without a fundamentally different signal.
- **common_words guard expansion** to catch title words: `capitalism` is rank 12270 (beyond top-10k), and `power`/`peace`/`nation` got subtracted because they're also gazetteer surnames. Rebuilding common_words to include frequent surnames would block legit `Young`/`King`/`Baker` recovery. Rejected — structural quote/STOP guards are the right tool.
- **Structural marker gate at ≥2 distinct keys / any bracketed**: caused `Garcia`→Garci, `Lloyd`→Lloy on real bracketed bylines. The current gate (majority-of-tokens-end-in-a-d, ≥2 distinct) is the safe version.
- **Semicolon global coauthor recovery**: shelved (section 5A) — low yield, over-extraction risk.

---

## 7. The ask

1. **Reproduce** the measurement (section 3). Confirm you see 50 silents / 0 regressions.
2. Look hard at `citation_silent_failures.xlsx` and the `scratchpad_silents.py` dump.
3. For each residual class, either (a) propose a **concrete, generalizing repair rule** (with the exact signal and why it won't regress the 810 controls — especially the a-d-ending real bylines and academic-initials cites), or (b) argue it's a **gold-label error / true ambiguity** with evidence.
4. Highest-value targets in our view: the **big-list omissions** (17 → if solvable safely, biggest chunk) and the **model org-as-sole-author over-extracts** (a clean flagger rule might zero these). But we're probably wrong about what's tractable — that's why you're here.
5. Anything you propose, VALIDATE with the replay diff before claiming it works. Report back: the rule, the code location, the fix/regression delta.

Write your findings to `/home/adam/Projects/flowstate/scratchpad_sol_findings.md`. We'll read it and implement/validate together. If you get blocked on tooling, note it there.

Thank you — genuinely. Fresh eyes with max thinking is exactly what this needs.
