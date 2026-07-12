# Name & word gazetteers

`given_names.txt` (50,788) and `surnames.txt` (121,791) are folded (lowercased, ASCII-alphanumeric),
sorted, deduplicated name lists used by `flag::has_other_author_name` to distinguish a genuine
coauthor name from Title-Case citation text when cancelling the `et al.` review flag.

`common_words.txt` (7,863) is the top ~10,000 English words (Norvig / Google Trillion-Word Corpus,
`count_1w.txt`) folded, with every name in the two gazetteers removed — so it holds only common
words that are *not* names (`considerations`, `institution`, `nuclear`). `snap::recover_*_coauthors`
consults it to refuse recovering a mis-parsed title/affiliation word as a coauthor. Frequency is the
discriminator: a real surname absent from the gazetteer sits deep in the word-frequency tail
(`kristensen` rank 69k), while a title/affiliation word ranks near the top (`considerations` 6.4k).
Regenerate: take the top-10k `count_1w.txt` words with length ≥ 3, fold, and subtract the union of
`given_names.txt` and `surnames.txt`. Per *Feist*, word/name frequency lists are uncopyrightable facts.

## Source & license

Derived from the multinational [`name-dataset`](https://github.com/philipperemy/name-dataset)
(~730k first names / ~983k surnames across 100+ countries, with per-country frequency rank). Only
the **flat name strings** are vendored here — no frequency, country, or gender data — which are
uncopyrightable facts (a name list is not a creative work; cf. *Feist v. Rural*). Frequency was the
essential ingredient: it separates real names (`Timothy` rank 206) from Title-Case word/place tokens
that are technically names somewhere (`Harvard`, `San`, `Economics`), which every zero-frequency
open source (Wikidata labels, Wiktionary, sigpwned CC0) could not.

## Regeneration

The lists are the frequency-filtered subset: given names with min per-country rank `< 1000`,
surnames with min rank `< 2000`. To regenerate after a dataset update, download
`names_dataset/v3/{first,last}_names.pkl.gz`, load with a **restricted unpickler** (the pickles are
pure data — verify no non-data classes), apply the thresholds, fold, sort, dedupe. (A build-time
script was used once; see git history / session notes.)
