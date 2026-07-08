# Citation labeling prompt

Parse the debate citation into the target JSON schema. Return only one JSON
object. Do not include markdown. Use `status: "parsed"` for real citations and
`status: "reject"` for inputs that are not citations.

Rules:

- Preserve information that appears in the citation.
- Use `null` when a field is absent or cannot be inferred validly.
- If the input is not a citation at all, return the reject object. Do not force
  non-citation text into the parsed citation schema.
- Preserve author order. `authors[0]` is the first named author, `authors[1]`
  is the second named author, and so on.
- Put each author's qualifications in that author's `qualifications` array when
  the cite clearly associates the qualification with that author.
- Author qualifications often appear between the author name and the year, as
  in `Fehrenbach, American author and former head of..., 8`; assign that text to
  the author's `qualifications` and infer `year: 2008` when valid.
- If a qualification is present but cannot be assigned to a specific author,
  attach it to the most likely author and add `ambiguous_author_qualification`.
- Infer a four-digit year from debate shorthand when valid:
  - `00` through `30` means `2000` through `2030`.
  - `31` through `99` means `1931` through `1999`.
  - One-digit years like `9` usually mean `2009` when they appear immediately
    after an author/institution name.
  - `2k` means `2000`.
- If the citation contains conflicting date signals, choose the publication
  year when clear and add a warning.
- Repeated appearances of the same date are not conflicting date signals. Use
  `conflicting_dates` only when the date signals materially disagree.
- Treat `ND`, `N.D.`, `No Date`, and `no date` as no-date signals, not card
  signatures. Set `no_date: true`, leave `year: null`, and add `no_date`.
- Do not treat legal court abbreviations like `N.D. Cal.` or `S.D.N.Y.` as
  no-date signals.
- Do not invent titles, publications, author first names, or URLs.
- Debate cutter initials, team marks, and trailing annotations are not authors.
- Put cutter initials or card signatures in `card_signatures`. Examples include
  trailing `// AKONG`, `//ah`, `)KMM`, `]AR`, `Lucas`, `tiktokbussin15`, and
  similar tags appended after the cite. Signatures may be lowercase, mixed case,
  words, initials, numbers, or crude strings.
- Put non-signature debate notes in `debate_annotations`. Notes often begin with
  `*`, but may also be appended immediately after a signature with no separator.
  Examples include `*tables reconstructed from the PDF` and
  `*machine-translated by Opus 4.8`.
- Do not move bibliographic details into `debate_annotations`. If the citer
  appears to be naming the dissertation committee, publisher, issue, report
  series, podcast, or other source context, put it in the closest bibliographic
  field or `raw_tail`.
- If trailing text says `recut NAME`, treat `NAME` as an additional card
  signature unless the text clearly says something else.
- If a signature and note are fused together and cannot be separated
  confidently, put the whole tail in `debate_annotations`, add
  `signature_note_fused`, and add a possible signature to `card_signatures` only
  if it is clear.
- If the citation has article/body spillover, parse only the citation header,
  discard the spillover, and add `body_spillover` to `warnings`.
- When spillover is present, set `spillover_start_index` to the zero-based
  character index where spillover begins in the raw input and set
  `spillover_start_text` to the first short string at that point. Otherwise use
  `null` for both fields.
- Put leftover citation text that looks meaningful but cannot be confidently
  structured in `raw_tail`; otherwise use `null`.
- Use low confidence for incomplete cites like `Goodwin 12`, `Ibid`, or `ACLU`.
- Use `source_type: "unknown"` when type is unclear.

Reject examples:

- Argument tags, analytics, song lyrics, placeholders, or card headings that do
  not identify a source should use `status: "reject"`.
- `AT: Hotlines CP` should reject with `analytic_or_tag`.
- `Captivate your mind...` should reject with `not_a_citation`.
- `Ibid` or `Id.` should reject with `cross_reference_only` unless there is
  enough surrounding citation material to parse.

Allowed warning strings:

- `incomplete_citation`
- `ambiguous_author`
- `ambiguous_author_qualification`
- `ambiguous_year`
- `conflicting_dates`
- `body_spillover`
- `trailing_debate_annotation`
- `url_only`
- `cross_reference_only`
- `no_date`
- `malformed`
- `possible_card_signature`
- `signature_note_fused`
- `unparsed_tail`
- `multiple_titles`
- `database_only`
- `page_range_ambiguous`
- `source_type_ambiguous`
- `not_a_citation`

Input citation:

```text
{{fullcite}}
```
