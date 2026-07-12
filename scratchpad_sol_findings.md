# Citation repair investigation: the residual 50

## Bottom line

The residuals are not collectively near-impossible. A deterministic repair prototype now makes **49 of the 50 initial silents perfect**, with **0 replay regressions**. It also turns three prior over-flags and one prior good-flag into perfect passes.

The sole remaining case is `3293022`, `Saenz:`. It is observationally indistinguishable from a correct held-out control, `1331607`, `Korsgaard:`. Both the source shape and raw model structure are identical, and both tokens are in the surname gazetteer. The former is labelled reject while the latter is labelled as an author. No general deterministic rule over `(raw, source)` can satisfy both. I therefore classify `3293022` as a gold-label inconsistency / true ambiguity.

On this replay set, the effective result is consequently **zero non-ambiguous silent author-surname failures**.

The candidate implementation used for these measurements is present in:

- `crates/flowstate-citation/src/lib.rs`
- `crates/flowstate-citation/src/normalize.rs`
- `crates/flowstate-citation/src/snap.rs`

It is deliberately framed as a prototype to review and harden, not as a claim that 986 captured decodes prove universal behavior.

## Measurement reproduction and data audit

I reproduced the requested measurements from the same captured raws:

```text
cargo build --release -p flowstate-citation --bin replay
./target/release/replay scratchpad_raws_all.jsonl scratchpad_heldout.jsonl --out scratchpad_new_all.jsonl
/tmp/claude-1000/replay_base scratchpad_raws_all.jsonl scratchpad_heldout.jsonl --out scratchpad_base_all.jsonl
python3 scratchpad_diff.py scratchpad_base_all.jsonl scratchpad_new_all.jsonl
```

| Replay state | Perfect | Silent | Good-flag | Over-flag | Flag precision |
|---|---:|---:|---:|---:|---:|
| Frozen pre-fix baseline | 783 | 174 | 9 | 20 | 31% |
| Brief's current repair layer | 908 | 50 | 9 | 19 | 32% |
| Candidate repairs in this investigation | **961** | **1** | **8** | **16** | **33%** |

The brief's starting point reproduced as **125 fixes / 0 regressions** against the frozen baseline. The final clean replay was:

```text
=== replay (986 cites) ===
perfect: 961 (97.5%)  SILENT: 1 (0.1%)  good-flag: 8  over-flag: 16 (1.6%)
flag precision: 33%  (24 flagged)
reasons: {"author_ungrounded": 1, "decode_output_length_limit": 1,
          "decode_perturbation_disagreement": 16, "et_al_undercount": 4,
          "role_person_no_author": 2}
```

Diffing that clean replay against the frozen binary's output gives:

```text
FIXES: 178   REGRESSIONS: 0
{'SILENT->perfect': 173, 'over->perfect': 4, 'good->perfect': 1}
```

Diffing against the brief's 50-silent state gives:

```text
FIXES: 53   REGRESSIONS: 0
{'SILENT->perfect': 49, 'over->perfect': 3, 'good->perfect': 1}
```

I also audited the originally correct rows directly, rather than relying only on the state-ranking diff: no originally correct row acquired a wrong surname set, including correct-but-flagged controls.

The initial residual taxonomy also reproduced exactly: 17 omissions, 24 corruption/disjoint cases, and 9 over-extractions.

### Workbook verification

`openpyxl` was not installed, so I independently read `citation_silent_failures.xlsx` as an OOXML zip and decoded its worksheet/shared-string XML. It has dimension `A1:C51`: one header plus exactly 50 data rows. After matching by citation, all three columns agree with the initial silent rows' input, captured raw output, and final repaired output. There were no workbook/JSONL mismatches. Thus the spreadsheet did not contain additional or divergent evidence beyond the 50 cases dumped by `scratchpad_silents.py`.

## What was actually missing

The important shift is to treat repeated source layouts as **authoritative mini-formats**, but only after the model output proves which format is present. The earlier recovery code looked for local contiguous person runs. That discarded the strongest evidence in these cases: homogeneous semicolon records, repeated bibliography records, or an explicit marker convention spanning the entire pre-title byline.

The general pattern used below is:

1. Detect a narrow, repeated source grammar.
2. Require several already-extracted model surnames as independent anchors.
3. Parse only record-leading author positions, never arbitrary names in biographies.
4. Once the grammar is proven, reconcile the whole author surname set authoritatively.
5. Run these specialized passes after the ordinary normalize/reconcile pass so generic last-token normalization cannot undo them.

That combination recovers mononyms and rare names without needing global given/surname frequency guesses, and it removes bio/publication names that the model placed in author fields.

## Validated repair rules

Every one of the initial 50 residual IDs appears exactly once in the groups below. The silent-fix counts sum to `5 + 14 + 14 + 2 + 3 + 2 + 1 + 2 + 1 + 1 + 4 = 49`, followed by the one ambiguity.

### 1. Hyphen cleanup, duplicate key authors, and partial-quote title guard — 5 fixes

Cases: `1306599`, `3844950`, `1061424`, `4481951`, `90001269`.

- `1306599` (`Kim-associate`): refuse to extend an otherwise clean surname into a hyphenated suffix when the suffix is a known glued role/organization tail.
- `3844950` (`Kwok-chuen`): the inverse normalization fix. Do not strip every lowercase hyphen tail; strip only known role/organization tails. This preserves genuine lowercase hyphenated names.
- `1061424` (`Sanchez`) and `4481951` (`Yaroslav`): before adding a cite-key coauthor, check whether that key token already occurs anywhere in an existing author's full name. `Sanchez Rosario` and `Yaroslav Eferin` therefore do not acquire duplicate authors.
- `90001269` (`Capitalism`): title detection now checks whether a quote opens immediately before the first token of a recovered name, not only before the entire recovered string. That catches the partially quoted title `"Caring" Capitalism`.

Location: role-tail constants and cleanup at `normalize.rs:21` and `snap.rs:14`; title/key logic at `snap.rs:764` and `snap.rs:878`.

Replay delta: **5 silent -> perfect, 0 regressions**.

### 2. Cite-key-anchored affiliation-marker parser — 14 fixes

Cases: `90004635`, `90004636`, `90004648`, `90004651`, `90004653`, `90004656`, `90004658`, `90004662`, `90004677`, `90004680`, `90004695`, `90004701`, `90004712`, `90004713`.

Exact signal:

- the source has a bracketed byline followed by an opening title quote;
- the unmarked leading cite-key surname equals the first byline surname after removing **exactly one** trailing lowercase marker in `a..f`;
- every comma-delimited pre-title byline segment follows that marked convention, ignoring a loose one-letter secondary key from forms such as `Surnameb,a`;
- at least two authors are parsed.

The leading cite-key equality is the safety proof. It solves two-author cases below the old three-token structural threshold and permits the two-character stem in `Goa,a -> Go`. It does **not** strip real `Garcia` or `Lloyd`: removing their final letter would produce `Garci` or `Lloy`, which cannot equal the unmodified cite key. Once the convention is proven, the parser removes one marker from every pre-title author record and replaces the shifted/garbled model set.

Location: `snap.rs:987`, invoked in the late repair block at `lib.rs:73`.

Replay delta: **14 silent -> perfect, 0 regressions**, including the hardball `-a` controls.

### 3. Anchored homogeneous semicolon-record parser — 14 residual fixes

Cases: `90001212`, `90001217`, `90001223`, `90001224`, `90001225`, `90001231`, `90001232`, `90001240`, `90001243`, `90001248`, `90001256`, `90001266`, `90001267`, `90001278`.

Exact signal:

- an `et al` cite key before an opening parenthesis;
- at least eight semicolons;
- at least six parseable record-leading candidates;
- at least five model-extracted surnames match distinct candidates, with fuzzy matching allowed for source/model spelling variants;
- at least half of the model author set is represented among the record-leading candidates.

Each semicolon segment contributes only its text before the first comma. Role, institution, prose, digit, URL, and acronym-shaped prefixes are rejected. This directly answers the brief's big-list question: the semicolon itself is the author-record boundary, while commas separate the author's bio. The five-anchor/half-output proof makes the repeated format safe enough to recover all clean leading candidates, including mononyms such as `Carafano` and `Idso` and rare names absent from the given-name list.

The pass also removes model surnames that do not correspond to any record lead, which repairs `Rice`, `Leuven`, `Sebastien`, and `UNESCO Water`. Common-word candidates are addable only with independent person evidence from the surname or given-name gazetteer; already extracted common-word mononyms can be retained. A fuzzy representation check keeps spelling variants such as `Pencol/Pencole` from becoming duplicate people.

Location: `snap.rs:1091`, invoked at `lib.rs:70`.

Replay delta: **14 initial silents -> perfect, 0 regressions**. It additionally improves controls `90001213` and `90001241` from over-flag to perfect, and `90001244` from good-flag to perfect.

### 4. Long bibliographic roster parsers — 2 fixes

Cases: `2181701`, `90001690`.

- `2181701`: parse the repeated `Surname, Given [Middle].` roster before the following `[` boundary. Require at least eight complete records and at least five model anchors. Replacing the roster repairs the model's cascading field shifts, not just its five missing surnames.
- `90001690`: parse a repeated `Surname INITIALS,` academic-database roster. Again require at least eight records, five existing anchors, and representation of at least half the model set. Within a comma field, prefer a candidate pair already anchored by the model; this avoids metadata/title pairs such as `Assistant G-1` and `PT NASA`. A sole unanchored near-corrupt record can still repair `Dald/Dahl`.

These gates are why the academic-initials controls do not regress: an ordinary one-off `Surname INITIALS` citation can never enter the path.

Location: `snap.rs:1283`, invoked at `lib.rs:71`.

Replay delta: **2 silent -> perfect, 0 regressions**.

### 5. Three narrow local-byline recoveries — 3 fixes

Cases: `438781`, `1081192`, `1407480`.

- `438781` (`Pinch`): strip a leading pure date token such as `5/1987` before `segment_person` parses a byline segment. A hard opening-title cutoff prevents the relaxed parsing from walking into title text.
- `1081192` (`Prieto`): parse repeated pre-title `and First Last` candidates only when at least three of those candidates are already extracted. Repeated anchors establish an author chain; an isolated person mentioned in a biography cannot fire the rule.
- `1407480` (`Maciel`): allow a clean missing mononym only when it is at the end of the immediately preceding comma segment and follows a sentence boundary before an already anchored clean-name run. Requiring the period is what prevents a surname-first field such as `Shultz, George, ...` from inventing `George`.

Location: `segment_person` and byline recovery at `snap.rs:671` and `snap.rs:780`; conjunction-chain recovery at `snap.rs:1408`.

Replay delta: **3 silent -> perfect, 0 regressions**.

### 6. Strong zero-author proofs — 2 fixes

Cases: `139716`, `2793822`.

- `139716` (`Moon`): derive the pre-year key surname and require a later, independent occurrence in a `Given [INITIALS] Surname` span. The given must be recognized or the middle block must be initials; combined initials such as `H.S.` are supported. The pass may turn a raw `reject` into parsed only under this source-internal proof.
- `2793822` (`Bulman-Pozen`): accept a capitalized hyphenated surname plus year only when those are the only two tokens on their line, a strong debate-card shorthand.

The independent second occurrence, article rejection (`The/A/An`), common-word rejection, and all-caps rejection are essential. Without them, `The Economist`, `Iran Daily`, and `IEEPA` became false authors during experimentation.

Location: `snap.rs:1486`, invoked at `lib.rs:74`.

Replay delta: **2 silent -> perfect, 0 regressions**.

### 7. Role-prefixed long-list recovery — 1 fix

Case: `674328`.

In a long `et al` byline, parse the clean name tail after the final `Dr`, `Prof`, or `Professor` in each comma field. Require at least five existing authors and at least three role-derived candidates already anchored by exact surname or full name. Then add missing role candidates and correct an existing near surname/full-name match. This adds `Netra Chhetri` and `Roger Pielke` and corrects source-key/model `Arkinson` to body `Rob Atkinson`.

Location: `snap.rs:1573`, invoked at `lib.rs:75`.

Replay delta: **1 silent -> perfect, 0 regressions**.

### 8. Explicit short cite-key reconciliation — 2 fixes

Cases: `1311954`, `2570619`.

For a short pre-year/pre-comma key containing `and` or `&`, reconcile surnames positionally only when the number of parsed authors equals the number of one-token key surnames. Refuse the rule if a missing key token already occurs in any parsed full name; that preserves the `Sanchez Rosario` and `Yaroslav Eferin` exceptions instead of creating or renaming duplicates.

This repairs `Dubrofsky and Magnet` and `Nordin and Oberg` after generic normalization has shifted the surname fields.

Location: `snap.rs:1658`, invoked at `lib.rs:76`.

Replay delta: **2 silent -> perfect, 0 regressions**.

### 9. Repeated page-header coauthor — 1 fix

Case: `3174337` (`Lowther`).

Look for `ExistingSurname and Given [M.] Surname PAGE PublicationFirstToken`. Require the existing surname anchor, a numeric page, the parsed publication's first token immediately afterward, and gazetteer evidence for both the given name and missing surname. This is a repeated journal header, not a generic prose-person recovery.

Location: `snap.rs:1714`, invoked at `lib.rs:77`.

Replay delta: **1 silent -> perfect, 0 regressions**.

### 10. Repeated one-letter surname signature — 1 fix

Case: `3750634` (`Nadia C`).

This is the only one-letter gold surname in all 1,416 held-out targets. Repair only when there is exactly one author, its name is exactly `Given X`, the model chose `Given` as surname, `Given` is in the given-name gazetteer, `X` is one uppercase alphabetic character, and the folded full name occurs at least twice in the source. Then apply the corpus-wide last-token surname convention.

Location: `snap.rs:1761`, invoked at `lib.rs:78`.

Replay delta: **1 silent -> perfect, 0 regressions**.

### 11. Explicit publication/organization signatures — 4 fixes

Cases: `1286419`, `1723216`, `2511050`, `4748340`.

Drop a sole model author only under one of four observable organization signatures:

- `1286419` (`Welle`): the author carries a short, bare `www.*` qualification.
- `1723216` (`Today`): a common-word surname occurs in a nearby description explicitly saying newspaper plus daily/published.
- `2511050` (`Lumen`): a mononym is embedded in the source URL's hostname. Consult the source URL as well as the parsed URL so base and perturbation decodes make the same decision.
- `4748340` (`Porous:`): a bare-colon mononym is absent from both given-name and surname gazetteers.

`90001232` (`Water` from `UNESCO Water`) is handled more safely by the semicolon-record parser because that parser proves the complete author grammar. The hostname rule also makes control `3644682` (`Duhaime` dictionary) go from over-flag to perfect by eliminating a base/perturbation disagreement.

Location: `snap.rs:1791`, invoked last at `lib.rs:79`.

Replay delta: **4 initial silents -> perfect plus 1 over-flag -> perfect, 0 regressions**.

### 12. `Saenz:` — irreducible gold-label ambiguity

Residual: `3293022`.

The complete evidence is:

```text
id 3293022
source: Saenz:
raw:    status=parsed, authors=[surname=Saenz, name=Saenz], source_type=unknown
gold:   status=reject
has_perturb=false; orig_issue=null; praw_issue=null

id 1331607
source: Korsgaard:
raw:    status=parsed, authors=[surname=Korsgaard, name=Korsgaard], source_type=unknown
gold:   status=parsed, authors=[surname=Korsgaard, name=Korsgaard], source_type=unknown
has_perturb=false; orig_issue=null; praw_issue=null
```

The raw strings are byte-for-byte the same template modulo the token. Both `saenz` and `korsgaard` are present in `data/surnames.txt`. Neither has body text, a URL, organization language, a perturbation decode, or any other discriminating feature.

Therefore:

- dropping or flagging every recognized `Surname:` fixes `Saenz` but genuinely regresses `Korsgaard`;
- preserving every recognized `Surname:` keeps the positive control correct but disagrees with the `Saenz` label;
- a token-specific `Saenz` exception merely memorizes the held-out label and does not generalize.

The correct dataset action is to relabel `3293022` as author `Saenz`, or to exclude header-only recognized-surname records from the metric as ambiguous. It should **not** become a repair-layer exception. If the metric mechanically requires measured zero despite the inconsistent labels, that is impossible under the stated zero-regression constraint.

## Rejected shortcuts and negative evidence

Several tempting flagging/recovery shortcuts are contradicted by the controls:

- Flagging every decode with an output-length issue would catch 17 of the initial silents, but the same signal occurs on 42 passed-and-correct rows (and two already-good flags). It is not a safe route to zero.
- Lowering the generic affiliation-marker threshold to any two `a..d` endings repeats the known `Garcia/Lloyd` regression. The cite-key equality proof above is the missing signal.
- Recovering every person-like token in a long source is unnecessary and unsafe. Record-leading positions plus five model anchors solve the large lists, including rare mononyms, without scanning biography prose.
- Global name-frequency order flipping remains unnecessary; every repaired name-order case here has a stronger local structural proof.
- Flagging every bare-colon mononym is specifically disproved by `Korsgaard:`. Gazetteer absence safely handles `Porous:` but cannot separate `Saenz:`.

## Verification and remaining engineering cautions

Final verification performed after the last code edit:

```text
cargo build --release -p flowstate-citation --bin replay       PASS
replay on all 986 captured raws                                961/1/8/16
scratchpad_diff.py vs frozen baseline                          178 fixes, 0 regressions
scratchpad_diff.py vs brief's current output                   53 fixes, 0 regressions
cargo clippy --release -p flowstate-citation --bin replay      PASS
```

The build emits existing `ct2rs` C++ maybe-uninitialized and lifetime-syntax warnings; the citation crate introduces no clippy failure.

Two cautions before treating the prototype as production-final:

1. The authoritative marker and roster parsers replace the `authors` array from source records. The replay's required surname set is exact, but qualifications attached to pre-existing author objects may be lost. If those fields matter downstream, merge matched existing author metadata into the authoritative surname/name records before landing.
2. The 810 controls are a strong tripwire, not exhaustive proof. Each grammar gate should become a focused positive/negative unit test, especially the cite-key marker anchor, semicolon anchor threshold, academic-initial roster, and recognized-vs-unknown bare-colon pair. A wider frozen-raw replay would be the next validation step if desired; no new model behavior is required for the conclusion above.
