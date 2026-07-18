//! SeekStorm-backed tub search index.
//!
//! Drop-in replacement for the Tantivy backend behind the same public surface
//! (`open` / `scan_and_index` / `search_files` / `search_content` /
//! `start_watcher`). The SeekStorm engine is async (Tokio); this module bridges
//! it to the synchronous `TubIndex` API with `block_on` at the wrapper boundary
//! — the tub scan already runs on GPUI's background executor, not a Tokio
//! thread, so blocking there is exactly what that thread is for.
//!
//! Decision ledger for the swap lives in the migration artifact; the load-
//! bearing picks that shaped this module:
//!   D1  UnicodeAlphanumericFolded index-wide (folds accents, keeps + - #).
//!   D3  deletes go by query on `file_id` (no user primary key; doc_id is
//!       engine-internal and we never store it).
//!   D4  ONE shared, worker-capped runtime — not one per index.
//!   D5  one hard commit at end of scan (+ close on shutdown).
//!   D6  vector field + Model2Vec + Hybrid, same PR (follow-on increment).
//!
//! The lexical path (open/scan/search) is on the product code path. The
//! vector/hybrid surface (`search_hybrid`, `create_vector_index`, the vector
//! schema + Model2Vec meta) is built and test-covered but deliberately off the
//! default path — the tub stays lexical until the per-unit embedding cost is
//! measured and opted into — so allow those intentionally-prod-unused items
//! rather than scatter per-item attributes.
#![allow(dead_code)]

use std::{
  fs,
  path::Path,
  sync::OnceLock,
};

use std::collections::HashSet;

use anyhow::{Result, anyhow};
use base64::Engine as _;
use seekstorm::{
  commit::Commit as _,
  index::{
    AccessType, Clustering, DeleteDocumentsByQuery as _, Document, DocumentCompression, FrequentwordType, IndexArc, IndexDocuments as _,
    IndexMetaObject, LexicalSimilarity, NgramSet, SchemaField, StemmerType, StopwordType, TokenizerType, create_index, open_index,
  },
  search::{FacetFilter, QueryRewriting, QueryType, ResultType, Search as _, SearchMode},
  vector::{Inference, Model, Quantization},
  vector_similarity::AnnMode,
};
use serde_json::{Value, json};
use tokio::runtime::{Handle, Runtime};

/// The on-disk index directory name — versioned so it coexists with the legacy
/// `tantivy-v2/` during the migration and a format change ships a fresh dir
/// rather than mis-reading an old one. The tub is regenerable, so a version
/// bump just triggers a re-scan.
pub(crate) const SEEKSTORM_DIR: &str = "seekstorm-v1";

// ---- Schema field names (shared with the projection/read side) --------------
// Same logical fields as the Tantivy schema, so hits round-trip identically.
// The hex-string encoding of Loro cursors becomes a real Binary field here, and
// `unit_kind` becomes a facet field (replacing the hand-built BooleanQuery kind
// union — the old T-S1 workaround).
pub(crate) mod field {
  pub const FILE_ID: &str = "file_id";
  pub const UNIT_ID: &str = "unit_id";
  pub const UNIT_KIND: &str = "unit_kind"; // facet
  pub const PATH: &str = "path";
  pub const DISPLAY_PATH: &str = "display_path";
  pub const FILE_NAME: &str = "file_name"; // completion source (autocomplete)
  pub const FILE_NAME_EXACT: &str = "file_name_exact";
  pub const HEADING_PATH: &str = "heading_path";
  pub const HEADING: &str = "heading";
  pub const CITE: &str = "cite";
  pub const BODY: &str = "body";
  pub const INSERT_TEXT: &str = "insert_text";
  pub const PARAGRAPH_START: &str = "paragraph_start"; // U32
  pub const PARAGRAPH_END: &str = "paragraph_end"; // U32
  pub const PARAGRAPH_START_CURSOR: &str = "paragraph_start_cursor"; // Binary
  pub const PARAGRAPH_END_CURSOR: &str = "paragraph_end_cursor"; // Binary
  pub const SIZE_BYTES: &str = "size_bytes"; // U64
  pub const MODIFIED_NS: &str = "modified_ns"; // U64
}

/// The tub schema as SeekStorm sees it. Built by JSON (the idiomatic path — the
/// private `field_id`/`indexed_field_id` are `#[serde(skip)]`). Field roles:
/// - `file_id` is `index_lexical` so a delete-by-query can select all of a
///   file's units by its id (D3 — SeekStorm has no user primary key).
/// - `unit_kind` is a String16 facet: the kind filter is a `facet_filter`, not a
///   hand-built boolean union.
/// - `file_name` is a `completion_source` for autocomplete; also lexically
///   searched.
/// - cursors are real `Binary`; paragraph offsets are real `U32`.
fn schema_json() -> &'static str {
  r#"
  [
    {"field":"file_id",                "field_type":"Text",   "store":true,  "index_lexical":true},
    {"field":"unit_id",                "field_type":"Text",   "store":true,  "index_lexical":false},
    {"field":"unit_kind",              "field_type":"String16","store":true, "index_lexical":false, "facet":true},
    {"field":"path",                   "field_type":"Text",   "store":true,  "index_lexical":false},
    {"field":"display_path",           "field_type":"Text",   "store":true,  "index_lexical":true},
    {"field":"file_name",              "field_type":"Text",   "store":true,  "index_lexical":true,  "completion_source":true},
    {"field":"file_name_exact",        "field_type":"Text",   "store":true,  "index_lexical":true},
    {"field":"heading_path",           "field_type":"Text",   "store":true,  "index_lexical":false},
    {"field":"heading",                "field_type":"Text",   "store":true,  "index_lexical":true,  "boost":2.0},
    {"field":"cite",                   "field_type":"Text",   "store":true,  "index_lexical":true},
    {"field":"body",                   "field_type":"Text",   "store":true,  "index_lexical":true,  "longest":true},
    {"field":"insert_text",            "field_type":"Text",   "store":true,  "index_lexical":false},
    {"field":"paragraph_start",        "field_type":"U32",    "store":true,  "index_lexical":false},
    {"field":"paragraph_end",          "field_type":"U32",    "store":true,  "index_lexical":false},
    {"field":"paragraph_start_cursor", "field_type":"Binary", "store":true,  "index_lexical":false},
    {"field":"paragraph_end_cursor",   "field_type":"Binary", "store":true,  "index_lexical":false},
    {"field":"size_bytes",             "field_type":"U64",    "store":true,  "index_lexical":false},
    {"field":"modified_ns",            "field_type":"U64",    "store":true,  "index_lexical":false}
  ]
  "#
}

fn schema_fields() -> Result<Vec<SchemaField>> {
  serde_json::from_str(schema_json()).map_err(|error| anyhow!("building tub SeekStorm schema: {error}"))
}

/// Vector-enabled variant of the schema (D6): `heading` and `body` also carry
/// `index_vector`, so Model2Vec generates an embedding per unit from that text.
/// Fields stay `index_lexical` too, which is what hybrid (lexical + vector)
/// requires. Not the default — enabling it makes every scan embed, a measured
/// cost the app opts into rather than pays unconditionally.
fn schema_json_vectors() -> String {
  schema_json()
    .replace(
      r#"{"field":"heading",                "field_type":"Text",   "store":true,  "index_lexical":true,  "boost":2.0}"#,
      r#"{"field":"heading",                "field_type":"Text",   "store":true,  "index_lexical":true,  "index_vector":true, "boost":2.0}"#,
    )
    .replace(
      r#"{"field":"body",                   "field_type":"Text",   "store":true,  "index_lexical":true,  "longest":true}"#,
      r#"{"field":"body",                   "field_type":"Text",   "store":true,  "index_lexical":true,  "index_vector":true, "longest":true}"#,
    )
}

fn schema_fields_vectors() -> Result<Vec<SchemaField>> {
  serde_json::from_str(&schema_json_vectors()).map_err(|error| anyhow!("building tub SeekStorm vector schema: {error}"))
}

/// Vector-enabled meta (D6): Model2Vec internal inference. `PotionBase2M` is the
/// smallest static model (CPU-only, no GPU); scalar-I8 quantization keeps the
/// vectors compact.
fn index_meta_vectors() -> IndexMetaObject {
  IndexMetaObject {
    inference: Inference::Model2Vec { model: Model::PotionBase2M, chunk_size: 1000, quantization: Quantization::ScalarQuantizationI8 },
    ..index_meta()
  }
}

/// Index-global config. No `Default`/builder exists — all fields are explicit.
/// D1: `UnicodeAlphanumericFolded` (folds accents, keeps `+ - #`). BM25F so the
/// per-field boosts in the schema take effect.
fn index_meta() -> IndexMetaObject {
  IndexMetaObject {
    id: 0,
    name: "flowstate-tub".to_string(),
    lexical_similarity: LexicalSimilarity::Bm25f,
    tokenizer: TokenizerType::UnicodeAlphanumericFolded,
    stemmer: StemmerType::None,
    stop_words: StopwordType::None,
    frequent_words: FrequentwordType::English,
    ngram_indexing: NgramSet::SingleTerm as u8,
    document_compression: DocumentCompression::Zstd,
    access_type: AccessType::Mmap,
    spelling_correction: None,
    query_completion: None,
    clustering: Clustering::None,
    inference: Inference::None,
  }
}

/// Open an existing index or create a fresh one at `index_path`. Mirrors the
/// Tantivy open-or-create: a versioned dir that already holds an index is
/// reopened; otherwise it is created with our schema.
pub(crate) fn open_or_create(index_path: &Path) -> Result<IndexArc> {
  block_on(async {
    if is_populated_dir(index_path) {
      return open_index(index_path).await.map_err(|error| anyhow!("opening SeekStorm index {}: {error}", index_path.display()));
    }
    fs::create_dir_all(index_path).map_err(|error| anyhow!("creating SeekStorm index dir {}: {error}", index_path.display()))?;
    let schema = schema_fields()?;
    // segment_number_bits1 = 11 (2048 segments) is the value used across the
    // SeekStorm examples/tests; mute = true keeps the library quiet.
    create_index(index_path, index_meta(), &schema, &Vec::new(), 11, true, None)
      .await
      .map_err(|error| anyhow!("creating SeekStorm index {}: {error}", index_path.display()))
  })
}

/// Create a fresh vector-enabled index (D6) — Model2Vec inference + the
/// `index_vector` schema. Separate from `open_or_create` so the lexical tub is
/// never forced to embed until the vector cost is measured and opted into.
pub(crate) fn create_vector_index(index_path: &Path) -> Result<IndexArc> {
  block_on(async {
    fs::create_dir_all(index_path).map_err(|error| anyhow!("creating vector index dir {}: {error}", index_path.display()))?;
    let schema = schema_fields_vectors()?;
    create_index(index_path, index_meta_vectors(), &schema, &Vec::new(), 11, true, None)
      .await
      .map_err(|error| anyhow!("creating SeekStorm vector index {}: {error}", index_path.display()))
  })
}

fn is_populated_dir(path: &Path) -> bool {
  fs::read_dir(path).map(|mut entries| entries.next().is_some()).unwrap_or(false)
}

/// Encode raw bytes for a `Binary` field (SeekStorm stores a base64 string).
pub(crate) fn encode_binary(bytes: &[u8]) -> String {
  base64::engine::general_purpose::STANDARD.encode(bytes)
}

pub(crate) fn decode_binary(text: &str) -> Option<Vec<u8>> {
  base64::engine::general_purpose::STANDARD.decode(text).ok()
}

/// Normalize a filename into space-separated words so the tokenizer emits
/// per-word tokens. SeekStorm's Folded tokenizer keeps `-`/`_` *inside* a token
/// (the same rule that preserves `c++`), so a raw name like `1AR---BioD__hash`
/// would index as one giant token and a word query ("biod") could never match.
/// Splitting on non-alphanumerics restores whole-word matching — what filename
/// search needs. The raw name is kept verbatim in `file_name_exact` for display.
fn filename_search_text(name: &str) -> String {
  name
    .chars()
    .map(|character| if character.is_alphanumeric() { character } else { ' ' })
    .collect::<String>()
    .split_whitespace()
    .collect::<Vec<_>>()
    .join(" ")
}

fn build_document(pairs: Vec<(&'static str, Value)>) -> Result<Document> {
  let mut map = serde_json::Map::with_capacity(pairs.len());
  for (key, value) in pairs {
    map.insert(key.to_string(), value);
  }
  serde_json::from_value(Value::Object(map)).map_err(|error| anyhow!("building SeekStorm document: {error}"))
}

/// File-level document (kind `File`) — mirrors the Tantivy `file_document`.
pub(crate) fn document_from_file(input: &super::FileDocumentInput<'_>) -> Result<Document> {
  build_document(vec![
    (field::FILE_ID, json!(input.file_id)),
    (field::UNIT_ID, json!(format!("{}:file", input.file_id))),
    (field::UNIT_KIND, json!(super::SearchUnitKind::File.as_str())),
    (field::PATH, json!(input.path.to_string_lossy())),
    (field::DISPLAY_PATH, json!(input.display_path)),
    (field::FILE_NAME, json!(filename_search_text(input.file_name))),
    (field::FILE_NAME_EXACT, json!(input.file_name)),
    (field::HEADING, json!(input.file_name)),
    (field::CITE, json!(input.kind.as_str())),
    (field::SIZE_BYTES, json!(input.size_bytes)),
    (field::MODIFIED_NS, json!(input.modified_ns)),
  ])
}

/// Content-unit document — mirrors the Tantivy `unit_document`. Numeric offsets
/// are real `U32`, cursors real `Binary`; both are omitted when absent rather
/// than stored as empty strings.
pub(crate) fn document_from_unit(unit: &super::IndexUnit) -> Result<Document> {
  let mut pairs: Vec<(&'static str, Value)> = vec![
    (field::FILE_ID, json!(unit.file_id)),
    (field::UNIT_ID, json!(unit.unit_id)),
    (field::UNIT_KIND, json!(unit.unit_kind.as_str())),
    (field::PATH, json!(unit.path.to_string_lossy())),
    (field::DISPLAY_PATH, json!(unit.display_path)),
    (field::FILE_NAME, json!(filename_search_text(&unit.file_name))),
    (field::FILE_NAME_EXACT, json!(unit.file_name)),
    (field::HEADING_PATH, json!(unit.heading_path.join(" / "))),
    (field::HEADING, json!(unit.heading)),
    (field::CITE, json!(unit.cite.as_deref().unwrap_or(""))),
    (field::BODY, json!(unit.body)),
    (field::INSERT_TEXT, json!(unit.insert_text)),
    (field::SIZE_BYTES, json!(unit.insert_text.len() as u64)),
    (field::MODIFIED_NS, json!(0u64)),
  ];
  if let Some(start) = unit.paragraph_start.and_then(|value| u32::try_from(value).ok()) {
    pairs.push((field::PARAGRAPH_START, json!(start)));
  }
  if let Some(end) = unit.paragraph_end_exclusive.and_then(|value| u32::try_from(value).ok()) {
    pairs.push((field::PARAGRAPH_END, json!(end)));
  }
  if let Some(bytes) = unit.paragraph_start_cursor.as_deref().filter(|bytes| !bytes.is_empty()) {
    pairs.push((field::PARAGRAPH_START_CURSOR, json!(encode_binary(bytes))));
  }
  if let Some(bytes) = unit.paragraph_end_cursor.as_deref().filter(|bytes| !bytes.is_empty()) {
    pairs.push((field::PARAGRAPH_END_CURSOR, json!(encode_binary(bytes))));
  }
  build_document(pairs)
}

/// Delete every document belonging to `file_id` (D3 — no user primary key, so we
/// select by the lexically-indexed `file_id` field and let the engine resolve
/// the internal doc_ids). `length` is set high to catch every unit of a file.
async fn delete_file_docs(index: &IndexArc, file_id: &str) {
  index
    .delete_documents_by_query(
      file_id.to_string(),
      QueryType::Intersection,
      0,
      1_000_000,
      true,
      vec![field::FILE_ID.to_string()],
      Vec::new(),
      Vec::new(),
    )
    .await;
}

/// Apply one scan's worth of changes: drop the docs of every changed/removed
/// file, index the freshly-derived documents, then one hard commit (D5).
pub(crate) fn apply_scan(index: &IndexArc, deletes: &[String], docs: Vec<Document>) -> Result<()> {
  block_on(async {
    for file_id in deletes {
      delete_file_docs(index, file_id).await;
    }
    if !docs.is_empty() {
      index.index_documents(docs).await;
    }
    index.commit().await;
  });
  Ok(())
}

/// Lexical search — the wrapper's default path (D-parity with Tantivy).
pub(crate) fn search(
  index: &IndexArc,
  query: &str,
  allowed_kinds: &[super::SearchUnitKind],
  filename_only: bool,
  limit: usize,
) -> Result<Vec<super::SearchHit>> {
  search_with_mode(index, query, allowed_kinds, filename_only, limit, SearchMode::Lexical)
}

/// Hybrid search (D6) — lexical + Model2Vec vector, fused by RRF. Requires an
/// index built with vector inference (see `index_meta_vectors`); against a
/// lexical-only index it degrades to lexical.
pub(crate) fn search_hybrid(
  index: &IndexArc,
  query: &str,
  allowed_kinds: &[super::SearchUnitKind],
  filename_only: bool,
  limit: usize,
) -> Result<Vec<super::SearchHit>> {
  search_with_mode(
    index,
    query,
    allowed_kinds,
    filename_only,
    limit,
    SearchMode::Hybrid { similarity_threshold: None, ann_mode: AnnMode::All },
  )
}

/// Run a search and materialize `SearchHit`s (previews are hydrated later by the
/// caller, exactly as the Tantivy path did). The kind constraint is a native
/// `facet_filter`, not a hand-built boolean union (retires the old T-S1 shape).
fn search_with_mode(
  index: &IndexArc,
  query: &str,
  allowed_kinds: &[super::SearchUnitKind],
  filename_only: bool,
  limit: usize,
  mode: SearchMode,
) -> Result<Vec<super::SearchHit>> {
  let field_filter: Vec<String> = if filename_only {
    vec![field::FILE_NAME.to_string(), field::DISPLAY_PATH.to_string(), field::FILE_NAME_EXACT.to_string()]
  } else {
    vec![
      field::HEADING.to_string(),
      field::BODY.to_string(),
      field::CITE.to_string(),
      field::FILE_NAME.to_string(),
      field::DISPLAY_PATH.to_string(),
    ]
  };
  let facet_filter = if allowed_kinds.is_empty() {
    Vec::new()
  } else {
    vec![FacetFilter::String16 {
      field: field::UNIT_KIND.to_string(),
      filter: allowed_kinds.iter().map(|kind| kind.as_str().to_string()).collect(),
    }]
  };

  block_on(async {
    let result = index
      .search(
        query.to_string(),
        None,
        QueryType::Union, // OR across terms, matching Tantivy's default query parser
        mode,
        false,
        0,
        limit,
        ResultType::TopkCount,
        true,
        field_filter,
        Vec::new(),
        facet_filter,
        Vec::new(),
        QueryRewriting::SearchOnly,
      )
      .await;

    let reader = index.read().await;
    let mut hits = Vec::with_capacity(result.results.len().min(limit));
    for entry in result.results.iter().take(limit) {
      let Ok(document) = reader.get_document(entry.doc_id, true, &None, &HashSet::new(), &[]).await else {
        continue;
      };
      if let Some(hit) = hit_from_document(&document, entry.score) {
        hits.push(hit);
      }
    }
    Ok(hits)
  })
}

fn hit_from_document(document: &Document, score: f32) -> Option<super::SearchHit> {
  let text = |key: &str| document.get(key).and_then(Value::as_str).map(ToOwned::to_owned);
  let number = |key: &str| document.get(key).and_then(Value::as_u64).map(|value| value as usize);
  let cursor = |key: &str| document.get(key).and_then(Value::as_str).and_then(decode_binary);

  let unit_kind = super::SearchUnitKind::from_str(&text(field::UNIT_KIND)?)?;
  let heading_path = text(field::HEADING_PATH)
    .unwrap_or_default()
    .split(" / ")
    .filter(|part| !part.is_empty())
    .map(ToOwned::to_owned)
    .collect::<Vec<_>>();

  Some(super::SearchHit {
    file_id: text(field::FILE_ID)?,
    unit_id: text(field::UNIT_ID)?,
    unit_kind,
    path: std::path::PathBuf::from(text(field::PATH)?),
    display_path: text(field::DISPLAY_PATH)?,
    file_name: text(field::FILE_NAME_EXACT)?,
    heading_path,
    title: text(field::HEADING).unwrap_or_default(),
    cite: super::non_empty(text(field::CITE).unwrap_or_default()),
    snippet: super::preview_text(&text(field::BODY).unwrap_or_default(), 360),
    insert_text: text(field::INSERT_TEXT).unwrap_or_default(),
    preview_paragraphs: Vec::new(),
    score,
    paragraph_start: number(field::PARAGRAPH_START),
    paragraph_end_exclusive: number(field::PARAGRAPH_END),
    paragraph_start_cursor: cursor(field::PARAGRAPH_START_CURSOR),
    paragraph_end_cursor: cursor(field::PARAGRAPH_END_CURSOR),
  })
}

/// One process-wide Tokio runtime shared by every `TubIndex` (D4).
///
/// SeekStorm is async and parallelizes indexing across shards internally, so it
/// wants a real multi-thread executor; a runtime *per* index would spin a whole
/// worker pool per tub and get churned every time the app rebuilds the tub on a
/// root change. Worker-capped to stay polite on a shared machine (OOM history).
pub(crate) fn shared_runtime() -> &'static Runtime {
  static RUNTIME: OnceLock<Runtime> = OnceLock::new();
  RUNTIME.get_or_init(|| {
    tokio::runtime::Builder::new_multi_thread()
      .worker_threads(4)
      .thread_name("tub-seekstorm")
      .enable_all()
      .build()
      .expect("build the shared tub SeekStorm runtime")
  })
}

/// Run an async SeekStorm call to completion from synchronous wrapper code.
///
/// Safe because tub wrapper methods are invoked from GPUI's background executor
/// (or test threads), never from within the shared runtime's own worker threads,
/// so this never blocks a thread that is itself driving the runtime.
pub(crate) fn block_on<F: std::future::Future>(future: F) -> F::Output {
  debug_assert!(
    Handle::try_current().is_err(),
    "block_on called from inside an async context — would stall the runtime"
  );
  shared_runtime().block_on(future)
}

#[cfg(test)]
mod tests {
  use std::collections::HashSet;

  use seekstorm::{
    index::{Document, FileType, IndexDocument as _},
    search::{FacetFilter, QueryRewriting, QueryType, ResultType, SearchMode},
  };

  use super::*;

  fn doc(pairs: &[(&str, serde_json::Value)]) -> Document {
    let mut map = serde_json::Map::new();
    for (key, value) in pairs {
      map.insert((*key).to_string(), value.clone());
    }
    serde_json::from_value(serde_json::Value::Object(map)).expect("build SeekStorm document")
  }

  /// End-to-end proof the API recipe holds in-tree: create with our schema,
  /// index two docs (text + facet + numeric + binary), commit, run a lexical
  /// query with a kind facet filter, and read a stored field back.
  #[test]
  fn seekstorm_roundtrip_index_search_get() {
    let base = std::env::temp_dir().join(format!("flowstate-seekstorm-smoke-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let index_path = base.join(SEEKSTORM_DIR);

    let index = open_or_create(&index_path).expect("open/create SeekStorm index");

    block_on(async {
      let cursor = encode_binary(&[1u8, 2, 3, 4]);
      index
        .index_document(
          doc(&[
            (field::FILE_ID, serde_json::json!("file-a")),
            (field::UNIT_ID, serde_json::json!("file-a:card:1")),
            (field::UNIT_KIND, serde_json::json!("Card")),
            (field::HEADING, serde_json::json!("Warming impacts")),
            (field::BODY, serde_json::json!("rising seas displace coastal populations")),
            (field::INSERT_TEXT, serde_json::json!("rising seas displace coastal populations")),
            (field::SIZE_BYTES, serde_json::json!(42u64)),
            (field::PARAGRAPH_START, serde_json::json!(0u32)),
            (field::PARAGRAPH_START_CURSOR, serde_json::json!(cursor)),
          ]),
          FileType::None,
        )
        .await;
      index
        .index_document(
          doc(&[
            (field::FILE_ID, serde_json::json!("file-b")),
            (field::UNIT_ID, serde_json::json!("file-b:block:1")),
            (field::UNIT_KIND, serde_json::json!("Block")),
            (field::HEADING, serde_json::json!("Economy")),
            (field::BODY, serde_json::json!("rising costs strain households")),
            (field::INSERT_TEXT, serde_json::json!("rising costs strain households")),
          ]),
          FileType::None,
        )
        .await;
      index.commit().await;

      // "rising" is in both docs; the facet filter must keep only the Card.
      let result = index
        .search(
          "rising".to_string(),
          None,
          QueryType::Intersection,
          SearchMode::Lexical,
          false,
          0,
          10,
          ResultType::TopkCount,
          false,
          Vec::new(),
          Vec::new(),
          vec![FacetFilter::String16 { field: field::UNIT_KIND.to_string(), filter: vec!["Card".to_string()] }],
          Vec::new(),
          QueryRewriting::SearchOnly,
        )
        .await;

      assert_eq!(result.results.len(), 1, "facet filter narrows two 'rising' hits to the one Card");

      let reader = index.read().await;
      let fetched = reader
        .get_document(result.results[0].doc_id, false, &None, &HashSet::new(), &[])
        .await
        .expect("read stored fields back");
      assert_eq!(fetched.get(field::FILE_ID).and_then(|value| value.as_str()), Some("file-a"));
      let stored_cursor = fetched.get(field::PARAGRAPH_START_CURSOR).and_then(|value| value.as_str()).expect("cursor stored");
      assert_eq!(decode_binary(stored_cursor), Some(vec![1u8, 2, 3, 4]), "binary field round-trips through base64");
    });

    let _ = fs::remove_dir_all(&base);
  }

  /// D6 proof: hybrid (lexical + Model2Vec vector) surfaces a semantically
  /// related card that a lexical query — sharing no terms with it — cannot.
  /// Ignored by default: loads the Model2Vec model (heavier + first run may
  /// fetch weights). Run explicitly: `cargo test -p flowstate-tub -- --ignored`.
  #[test]
  #[ignore = "loads the Model2Vec model; run explicitly with --ignored"]
  fn seekstorm_hybrid_finds_semantic_match() {
    let base = std::env::temp_dir().join(format!("flowstate-seekstorm-hybrid-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let index_path = base.join("vectors");
    let index = create_vector_index(&index_path).expect("create vector index");

    let sea = "rising seas swallow coastal towns and drive families from their homes";
    let econ = "central banks raise interest rates to bring down consumer inflation";
    let unit = |id: &str, body: &str| {
      doc(&[
        (field::FILE_ID, serde_json::json!(id)),
        (field::UNIT_ID, serde_json::json!(format!("{id}:card:1"))),
        (field::UNIT_KIND, serde_json::json!(crate::SearchUnitKind::Card.as_str())),
        (field::PATH, serde_json::json!(format!("/{id}.db8"))),
        (field::DISPLAY_PATH, serde_json::json!(format!("{id}.db8"))),
        (field::FILE_NAME_EXACT, serde_json::json!(format!("{id}.db8"))),
        (field::HEADING, serde_json::json!(id)),
        (field::BODY, serde_json::json!(body)),
        (field::INSERT_TEXT, serde_json::json!(body)),
      ])
    };

    block_on(async {
      index.index_document(unit("sea", sea), FileType::None).await;
      index.index_document(unit("econ", econ), FileType::None).await;
      index.commit().await;
    });

    // A query that shares NO terms with the sea card but means the same thing.
    let query = "ocean flooding forces people off the shoreline";
    let kinds = [crate::SearchUnitKind::Card];
    let lexical = search(&index, query, &kinds, false, 10).expect("lexical search");
    let hybrid = search_hybrid(&index, query, &kinds, false, 10).expect("hybrid search");

    assert!(
      lexical.iter().all(|hit| hit.file_id != "sea"),
      "the sea card shares no query terms, so lexical alone should miss it (got {:?})",
      lexical.iter().map(|hit| &hit.file_id).collect::<Vec<_>>()
    );
    assert!(
      hybrid.iter().any(|hit| hit.file_id == "sea"),
      "hybrid should surface the semantically-related sea card via the vector leg (got {:?})",
      hybrid.iter().map(|hit| &hit.file_id).collect::<Vec<_>>()
    );

    let _ = fs::remove_dir_all(&base);
  }
}
