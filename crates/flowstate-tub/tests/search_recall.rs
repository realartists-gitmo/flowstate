//! The equivalence net for the Tantivy → SeekStorm search-backend migration.
//!
//! Backend-agnostic by construction: it drives only the public `TubIndex`
//! surface, so the SAME harness measures whichever engine sits behind it. It
//! pins the current engine's recall today and must still pass once SeekStorm is
//! swapped in on this branch — a regression here blocks the cutover.
//!
//! Method is KNOWN-ITEM RECALL — no human relevance labels required. For each
//! indexed item we derive a query from that item's OWN text and require the
//! engine to return it in the top-K. A *regression* is a known item the engine
//! can no longer find; different ranking or extra hits are *changes*, not
//! failures. That is the D2 change-vs-regression line made mechanical.
//!
//! The debate corpus is `.docx` (filename-searchable only — scan content-indexes
//! `.db8`/`.fl0`), so the active gate here is the FILENAME path, which is exactly
//! where the migration's risk concentrates (ngram tokenizer → SeekStorm
//! completion). The content path auto-enables when the sample contains `.db8`/
//! `.fl0` files; a dedicated content fixture is a tracked residual.
//!
//! Gated on `FLOWSTATE_CORPUS_DIR` (policy: search validation runs over the real
//! corpus). Tunables: `FLOWSTATE_RECALL_SAMPLE` (files, default 60),
//! `FLOWSTATE_RECALL_TOPK` (default 20).

use std::{
  fs,
  path::{Path, PathBuf},
};

use flowstate_tub::{SearchUnitKind, TubIndex};

const DEFAULT_SAMPLE: usize = 60;
const DEFAULT_TOPK: usize = 20;

fn env_usize(key: &str, default: usize) -> usize {
  std::env::var(key).ok().and_then(|value| value.parse().ok()).unwrap_or(default)
}

/// Lowercase alphabetic words of length >= 4 from `text`, longest first, deduped.
/// These are the known-item query seeds: distinctive enough to locate one item.
fn distinctive_words(text: &str) -> Vec<String> {
  let mut words: Vec<String> = text
    .split(|character: char| !character.is_alphabetic())
    .filter(|word| word.chars().count() >= 4)
    .map(str::to_lowercase)
    .collect();
  words.sort_by(|left, right| right.chars().count().cmp(&left.chars().count()).then_with(|| left.cmp(right)));
  words.dedup();
  words
}

/// Strip the tub's `__<hash>` suffix and the extension, leaving the human name.
fn filename_stem(name: &str) -> String {
  let no_ext = name.rsplit_once('.').map_or(name, |(head, _)| head);
  no_ext.rsplit_once("__").map_or(no_ext, |(head, _)| head).to_string()
}

/// Copy a deterministic, strided sample of corpus files into `dest`; return the
/// count copied. Deterministic (sorted + strided) so the net is stable run to
/// run — a moving target can't gate a cutover.
fn stage_sample(corpus: &Path, dest: &Path, want: usize) -> usize {
  let mut entries: Vec<PathBuf> = fs::read_dir(corpus)
    .expect("corpus dir readable")
    .filter_map(Result::ok)
    .map(|entry| entry.path())
    .filter(|path| {
      path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(ext, "docx" | "db8" | "fl0"))
    })
    .collect();
  entries.sort();
  if entries.is_empty() {
    return 0;
  }
  let stride = (entries.len() / want.max(1)).max(1);
  let mut copied = 0;
  for path in entries.iter().step_by(stride).take(want) {
    let Some(name) = path.file_name() else { continue };
    if fs::copy(path, dest.join(name)).is_ok() {
      copied += 1;
    }
  }
  copied
}

#[test]
fn known_item_recall_over_corpus() {
  let Some(corpus) = std::env::var_os("FLOWSTATE_CORPUS_DIR") else {
    eprintln!("FLOWSTATE_CORPUS_DIR unset; skipping search-recall net");
    return;
  };
  let corpus = PathBuf::from(corpus);
  let sample = env_usize("FLOWSTATE_RECALL_SAMPLE", DEFAULT_SAMPLE);
  let topk = env_usize("FLOWSTATE_RECALL_TOPK", DEFAULT_TOPK);

  let base = std::env::temp_dir().join(format!("flowstate-recall-{}", std::process::id()));
  let _ = fs::remove_dir_all(&base);
  let root = base.join("root");
  let data = base.join("data");
  fs::create_dir_all(&root).expect("create tub root");
  fs::create_dir_all(&data).expect("create data dir");

  let staged = stage_sample(&corpus, &root, sample);
  assert!(staged > 0, "the corpus sample staged at least one file");

  let index = TubIndex::open(&root, &data).expect("open tub index");
  let files = index.scan_and_index().expect("scan the staged sample");
  let indexed: Vec<_> = files.iter().filter(|file| file.indexed).collect();
  assert!(!indexed.is_empty(), "at least one staged file indexed");

  // ---- Filename known-item recall (the D2 autocomplete path) ----
  let mut fn_checked = 0usize;
  let mut fn_found = 0usize;
  let mut fn_misses: Vec<String> = Vec::new();
  for file in &indexed {
    let stem = filename_stem(&file.file_name);
    let Some(seed) = distinctive_words(&stem).into_iter().next() else {
      continue;
    };
    fn_checked += 1;
    let hits = index.search_files(&seed, topk).expect("filename search");
    if hits.iter().any(|hit| hit.file_id == file.file_id) {
      fn_found += 1;
    } else {
      fn_misses.push(format!("{} (seed \"{seed}\")", file.file_name));
    }
  }

  // ---- Content known-item recall (auto-enabled when db8/fl0 units exist) ----
  let kinds = [
    SearchUnitKind::Card,
    SearchUnitKind::BlockSection,
    SearchUnitKind::Analytic,
    SearchUnitKind::TagSection,
    SearchUnitKind::Hat,
    SearchUnitKind::Pocket,
    SearchUnitKind::Paragraph,
    SearchUnitKind::Cite,
    SearchUnitKind::FlowNode,
  ];
  let units = index.default_content(&kinds, 500).expect("enumerate content units");
  // Bound the search loop for runtime: stride to at most ~100 probes.
  let stride = (units.len() / 100).max(1);
  let mut ct_checked = 0usize;
  let mut ct_found = 0usize;
  let mut ct_misses: Vec<String> = Vec::new();
  for unit in units.iter().step_by(stride) {
    let seed = distinctive_words(&unit.title)
      .into_iter()
      .chain(distinctive_words(&unit.snippet))
      .next();
    let Some(seed) = seed else {
      continue;
    };
    ct_checked += 1;
    let hits = index
      .search_content(&seed, &[unit.unit_kind.clone()], topk)
      .expect("content search");
    // Docx unit_ids embed a per-import Loro id, so this live enumeration and the
    // indexed copy carry DIFFERENT ids for the same card. Match on stable content
    // identity (same file + same insertable text or title) instead.
    let found = hits.iter().any(|hit| {
      hit.file_id == unit.file_id
        && ((!unit.insert_text.trim().is_empty() && hit.insert_text.trim() == unit.insert_text.trim())
          || hit.title == unit.title)
    });
    if found {
      ct_found += 1;
    } else if ct_misses.len() < 20 {
      ct_misses.push(format!("\"{}\" (seed \"{seed}\")", unit.title));
    }
  }

  let fn_recall = fn_found as f64 / fn_checked.max(1) as f64;
  let ct_recall = ct_found as f64 / ct_checked.max(1) as f64;

  eprintln!("── search-recall net · backend baseline ──");
  eprintln!("staged {staged} files · {} indexed · top-K {topk}", indexed.len());
  eprintln!("filename known-item recall@{topk}: {fn_found}/{fn_checked} = {fn_recall:.3}");
  if ct_checked == 0 {
    eprintln!(
      "content path: no .db8/.fl0 units in this sample (corpus is docx-only). Filename path is the \
       active gate; content recall needs a db8/fl0 fixture — tracked residual."
    );
  } else {
    eprintln!("content  known-item recall@{topk}: {ct_found}/{ct_checked} = {ct_recall:.3}");
  }
  if !fn_misses.is_empty() {
    eprintln!("filename misses ({}, first 20):", fn_misses.len());
    for miss in fn_misses.iter().take(20) {
      eprintln!("  · {miss}");
    }
  }
  if !ct_misses.is_empty() {
    eprintln!("content misses (first {}):", ct_misses.len());
    for miss in &ct_misses {
      eprintln!("  · {miss}");
    }
  }

  let _ = fs::remove_dir_all(&base);

  // Conservative sanity FLOORS for the current engine — these pin a baseline,
  // not a target. Post-cutover the SeekStorm run must meet or beat the numbers
  // printed above (regression = recall drop beyond tolerance); the floors then
  // get tightened to the engine's observed level.
  // Floors sit just under the observed Tantivy baseline (filename 0.962, content
  // 0.920 over the corpus) so a real SeekStorm recall regression trips the net
  // while known-item noise (a digit splitting a seed word) does not.
  assert!(fn_checked > 0, "the filename path was actually exercised");
  assert!(
    fn_recall >= 0.90,
    "filename known-item recall {fn_recall:.3} below the Tantivy baseline floor — a distinctive \
     filename word should locate its file"
  );
  if ct_checked > 0 {
    assert!(ct_recall >= 0.85, "content known-item recall {ct_recall:.3} below the Tantivy baseline floor");
  }
}
