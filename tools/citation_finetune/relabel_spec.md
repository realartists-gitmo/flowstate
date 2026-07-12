# Citation relabel spec (Sonnet adjudication pass)

Frozen instruction set for the Sonnet relabel + the retrain target format. Derived from a
manual audit of ~204 model-vs-Haiku disagreements (see `grounded_but_wrong.xlsx`), where the
**model beat the Haiku label 45% of the time and tied 13%** — i.e. the Haiku gold carries
heavy systematic error. This pass fixes those labels and freezes three format simplifications.

## 1. Target format changes (this is a frozen-format retrain)

Four changes bundle into ONE retrain, each removing something the model is structurally bad at:

1. **Author name → single `surname`, drop the `family`/`given` split.** Debate keys on the
   surname/cite-tag (`Weidi 16` → the cite *is* "Weidi"), not the linguistic family-vs-given
   distinction. Emit one `surname` per author (the cite-reference surname) plus `name` (the full
   in-text name string, for quals/credibility). **Do not** emit a separate `given` field.
   - Kills the CJK name-order error class (no "which token is the family").
   - Kills the "given-as-family" gold errors (they only existed because of the rigid split).
2. **Spillover: drop `spillover_start_index`, keep `spillover_start_text`.** The model cannot
   emit reliable character offsets; it *can* copy the start string. The index is derived
   deterministically by matching the string in the source at inference time.
3. **Compact keys** (short field tags) — as already specified in `deployment_protocols.md` §5.
4. **Preserve source curly quotes** `“ ”` verbatim in titles/quals (never straight `"`), so
   inner quotes can't collide with the JSON delimiter.

## 2. Extraction rubric (apply to every relabel)

- **Title: always extract when present.** The #1 Haiku error was `title=None` when a
  title/headline/definition-word clearly exists (~45 of the model-beats-label cases). If the
  source has a title, populate it.
- **`surname` = the cite-reference surname**, spelled per the rule below. `name` = the full
  in-text name string.
- **Authors = the byline only.** Do NOT pull names from qualifications/bios: a person's boss,
  cited/reviewed scholars, the article's subject, or institution names (`William Smith` = a
  college). DO capture every real co-author, including the 2nd in "X and Y" bylines.
- **Long author lists (≥6 / "et al" + a name run): enumerate fully.** Both Haiku and the model
  systematically truncate these — get the complete list.

## 3. The four decisions (rubric rules)

1. **Organization byline → `publisher`, not `author`.** When the byline IS an org (EPA, Reuters,
   Merriam-Webster, Voice of America, Al Khaleej, International Panel), put it in
   `publisher`/`publication`; leave `authors[]` empty. Reserve authors for named people.
2. **Surname spelling → plurality → longest → body.** If the surname appears multiple times in
   the source, use the majority spelling. If tied, use the longest/most-complete form. If still
   unresolved, fall back to the full in-text (body) spelling. (Handles head-shorthand vs body
   conflicts like `Robison`/`Robinson`, `Hilborn`/`Hillborn`.)
3. **Correct typos — but conservatively.** Fix obvious typos in names AND titles (`Joesph`→
   `Joseph`). BUT be cautious about *deciding* something is a typo; if a name/word is merely
   unusual rather than clearly misspelled, keep it and **flag for human review** rather than
   "correcting" a real name.
4. **CJK / all names: surname only.** Per (1) in §1 — no family/given ordering call. `Xu Weidi`
   → surname `Weidi` if that's the cite tag (the head shorthand is the reference form).

## 4. Adjudication routing (three buckets)

The model was trained on Haiku, so it *replicates* Haiku's systematic errors — meaning
model≡Haiku agreement is NOT a correctness signal on the hard classes. Route accordingly:

- **Bucket 1 — Disagreements** (model output ≠ Haiku label on surname-set / source_type / title
  / author-count): Sonnet adjudicates against the source. **Full Sonnet pass.**
- **Bucket 2 — Agreements within known-hard classes** (long author lists, any spillover,
  org-byline): Sonnet reviews anyway — agreement here is likely *shared inherited error*.
  **Full Sonnet pass.**
- **Bucket 3 — Agreements on short/clean cites**: trust, but **random-audit a sample** to
  measure the residual error rate we accept without review.

## 5. Adjudication prompt shape (buckets 1 & 2)

Present Sonnet the **source** plus **two blind candidate extractions** (A and B — do NOT tell
it which is the old label vs the model; both may be wrong). Task: produce the correct
extraction per §2–§3, and report, per field, which candidate it agreed with (`A` / `B` /
`neither`) — this yields a structured map of Haiku-error-rate vs model-error-rate and validates
the "model beats label" finding at full scale. Surface (don't guess) anything genuinely
ambiguous (odd names, unclear org-vs-person, unresolved spelling).
