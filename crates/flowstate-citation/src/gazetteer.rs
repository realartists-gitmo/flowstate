//! Vendored frequency-filtered word lists, shared across the repair and flag layers.
//!
//! - [`GIVEN_NAMES`] / [`SURNAMES`]: multinational name gazetteers from the `name-dataset` (top-ranked
//!   names only — real names are common, word/place tokens like `Harvard` or `Economics` are absent).
//! - [`COMMON_WORDS`]: the top ~10k English words (Norvig / Google Trillion-Word Corpus) with every
//!   gazetteer name removed, so it holds only common words that are *not* names (`considerations`,
//!   `institution`, `nuclear`). Frequency is the discriminator: a real surname absent from the
//!   gazetteer sits deep in the word-frequency tail (`kristensen` rank 69k), while a mis-parsed
//!   title/affiliation word ranks near the top. Per *Feist*, word/name frequency lists are
//!   uncopyrightable facts; only the flat strings are vendored.

use std::collections::HashSet;
use std::sync::LazyLock;

fn load(text: &'static str) -> HashSet<&'static str> {
    text.lines().filter(|l| !l.is_empty()).collect()
}

/// Folded given-name gazetteer.
pub static GIVEN_NAMES: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| load(include_str!("../data/given_names.txt")));
/// Folded surname gazetteer.
pub static SURNAMES: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| load(include_str!("../data/surnames.txt")));
/// Folded common English words that are not names — used to reject a mis-parsed word as an author.
pub static COMMON_WORDS: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| load(include_str!("../data/common_words.txt")));

/// True when `folded` is a common English word that is not any known name — i.e. a mis-parsed
/// title/affiliation token (`considerations`, `nuclear`), not a real (if rare) surname.
pub fn is_common_word(folded: &str) -> bool {
    COMMON_WORDS.contains(folded)
}
