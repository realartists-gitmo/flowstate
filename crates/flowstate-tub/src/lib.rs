use std::{
  collections::{BTreeMap, BTreeSet, HashMap, HashSet},
  fs,
  hash::{Hash, Hasher as _},
  path::{Path, PathBuf},
  sync::mpsc::{self, Receiver},
  time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result};
use flowstate_document::{
  DocumentPackage, DocumentProjection, InputParagraph, InputRun, SearchUnitChunk, document_text_slice, paragraph_byte_range, paragraph_text_len,
  read_db8,
};
use ignore::WalkBuilder;
use notify::{
  Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as _,
  event::{DataChange, MetadataKind, ModifyKind},
};
use rusqlite::{Connection, OptionalExtension as _, params};
use serde::{Deserialize, Serialize};
use tantivy::{
  Index, IndexWriter, TantivyDocument, Term, doc,
  query::QueryParser,
  schema::{Field, IndexRecordOption, STORED, STRING, Schema, TEXT, TextFieldIndexing, TextOptions, Value as _},
  tokenizer::NgramTokenizer,
};

const CATALOG_FILE: &str = "catalog.sqlite3";
const TANTIVY_DIR: &str = "tantivy-v2";
const FILENAME_TOKENIZER: &str = "filename_ngram";
const WRITER_MEMORY_BYTES: usize = 96_000_000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum FileKind {
  Db8,
  Docx,
  Fl0,
}

impl FileKind {
  #[must_use]
  pub const fn as_str(self) -> &'static str {
    match self {
      Self::Db8 => "db8",
      Self::Docx => "docx",
      Self::Fl0 => "fl0",
    }
  }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum SearchUnitKind {
  File,
  Pocket,
  Hat,
  BlockSection,
  TagSection,
  Analytic,
  Card,
  Cite,
  Paragraph,
  ImageAlt,
  ImageCaption,
  Equation,
  TableCell,
  FlowNode,
  Document,
  Unknown(String),
}

impl SearchUnitKind {
  #[must_use]
  pub fn as_str(&self) -> &str {
    match self {
      Self::File => "file",
      Self::Pocket => "pocket",
      Self::Hat => "hat",
      Self::BlockSection => "block",
      Self::TagSection => "tag",
      Self::Analytic => "analytic",
      Self::Card => "card",
      Self::Cite => "cite",
      Self::Paragraph => "paragraph",
      Self::ImageAlt => "image_alt",
      Self::ImageCaption => "image_caption",
      Self::Equation => "equation",
      Self::TableCell => "table_cell",
      Self::FlowNode => "flow_node",
      Self::Document => "document",
      Self::Unknown(value) => value.as_str(),
    }
  }

  fn from_str(value: &str) -> Option<Self> {
    Some(match value {
      "file" => Self::File,
      "pocket" => Self::Pocket,
      "hat" => Self::Hat,
      "block" => Self::BlockSection,
      "tag" => Self::TagSection,
      "analytic" => Self::Analytic,
      "card" => Self::Card,
      "cite" => Self::Cite,
      "paragraph" => Self::Paragraph,
      "image_alt" => Self::ImageAlt,
      "image_caption" => Self::ImageCaption,
      "equation" => Self::Equation,
      "table_cell" => Self::TableCell,
      "flow_node" => Self::FlowNode,
      "document" => Self::Document,
      _ => Self::Unknown(value.to_owned()),
    })
  }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TubFile {
  pub file_id: String,
  pub path: PathBuf,
  pub display_path: String,
  pub parent_display_path: String,
  pub file_name: String,
  pub kind: FileKind,
  pub size_bytes: u64,
  pub modified_ns: u64,
  pub indexed: bool,
  pub last_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TubTreeNode {
  pub path: PathBuf,
  pub display_path: String,
  pub name: String,
  pub is_dir: bool,
  pub depth: usize,
  pub expanded: bool,
  pub file_kind: Option<FileKind>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SearchHit {
  pub file_id: String,
  pub unit_id: String,
  pub unit_kind: SearchUnitKind,
  pub path: PathBuf,
  pub display_path: String,
  pub file_name: String,
  pub heading_path: Vec<String>,
  pub title: String,
  pub cite: Option<String>,
  pub snippet: String,
  pub insert_text: String,
  #[serde(default)]
  pub preview_paragraphs: Vec<InputParagraph>,
  pub score: f32,
  pub paragraph_start: Option<usize>,
  pub paragraph_end_exclusive: Option<usize>,
  #[serde(default)]
  pub paragraph_start_cursor: Option<Vec<u8>>,
  #[serde(default)]
  pub paragraph_end_cursor: Option<Vec<u8>>,
}

#[derive(Clone, Debug)]
pub struct TubIndex {
  root: PathBuf,
  catalog_path: PathBuf,
  index: Index,
  schema: TubSchema,
}

impl TubIndex {
  pub fn open(root: impl AsRef<Path>, data_dir: impl AsRef<Path>) -> Result<Self> {
    let root = canonicalize_dir(root.as_ref())?;
    let data_dir = data_dir.as_ref().to_path_buf();
    fs::create_dir_all(&data_dir).with_context(|| format!("creating tub data directory {}", data_dir.display()))?;

    let catalog_path = data_dir.join(CATALOG_FILE);
    let index_dir = data_dir.join(TANTIVY_DIR);
    fs::create_dir_all(&index_dir).with_context(|| format!("creating Tantivy index directory {}", index_dir.display()))?;

    let (schema, fields) = build_schema();
    let index = match Index::open_in_dir(&index_dir) {
      Ok(index) => index,
      Err(_) => Index::create_in_dir(&index_dir, schema).with_context(|| format!("creating Tantivy index {}", index_dir.display()))?,
    };
    register_tokenizers(&index);

    let this = Self {
      root,
      catalog_path,
      index,
      schema: fields,
    };
    this.initialize_catalog()?;
    Ok(this)
  }

  #[must_use]
  pub fn root(&self) -> &Path {
    &self.root
  }

  pub fn scan_and_index(&self) -> Result<Vec<TubFile>> {
    let mut writer = None;
    let existing = self.files_by_path()?;
    let mut seen_paths = HashSet::new();
    let mut files = Vec::new();
    let mut pending_upserts = Vec::new();
    let mut pending_deletes = Vec::new();

    for entry in WalkBuilder::new(&self.root)
      .hidden(false)
      .git_ignore(true)
      .git_global(true)
      .git_exclude(true)
      .build()
    {
      let entry = match entry {
        Ok(entry) => entry,
        Err(_) => continue,
      };
      if !entry
        .file_type()
        .is_some_and(|file_type| file_type.is_file())
      {
        continue;
      }
      let path = entry.path();
      let Some(kind) = file_kind_from_path(path) else {
        continue;
      };

      let path = canonicalize_file(path)?;
      seen_paths.insert(path.clone());
      let metadata = fs::metadata(&path)?;
      let size_bytes = metadata.len();
      let modified_ns = modified_ns(&metadata);
      let display_path = display_path_for(&self.root, &path);
      let parent_display_path = parent_display_path(&display_path);
      let file_name = path
        .file_name()
        .map_or_else(|| display_path.clone(), |name| name.to_string_lossy().to_string());
      let fingerprint = fingerprint(size_bytes, modified_ns, kind, &path)?;
      let existing = existing.get(&path);
      let file_id = existing.map_or_else(|| stable_file_id(&self.root, &path), |record| record.file_id.clone());

      if let Some(existing) = existing
        && existing.kind == kind
        && existing.fingerprint == fingerprint
        && existing.indexed
      {
        files.push(existing.clone().into());
        continue;
      }

      let mut indexed = true;
      let mut last_error = None;
      let writer = self.index_writer(&mut writer)?;
      writer.delete_term(Term::from_field_text(self.schema.file_id, &file_id));
      writer.add_document(file_document(
        &self.schema,
        FileDocumentInput {
          file_id: &file_id,
          kind,
          path: &path,
          display_path: &display_path,
          file_name: &file_name,
          size_bytes,
          modified_ns,
        },
      ))?;

      if kind == FileKind::Db8 {
        match db8_index_units(&file_id, &path, &display_path, &file_name) {
          Ok(units) => {
            for unit in units {
              writer.add_document(unit_document(&self.schema, &unit))?;
            }
          },
          Err(error) => {
            indexed = false;
            last_error = Some(error.to_string());
          },
        }
      }

      let record = CatalogFileRecord {
        file_id: file_id.clone(),
        path: path.clone(),
        display_path: display_path.clone(),
        parent_display_path: parent_display_path.clone(),
        file_name: file_name.clone(),
        kind,
        size_bytes,
        modified_ns,
        fingerprint,
        indexed,
        last_error: last_error.clone(),
      };
      pending_upserts.push(record);

      files.push(TubFile {
        file_id,
        path,
        display_path,
        parent_display_path,
        file_name,
        kind,
        size_bytes,
        modified_ns,
        indexed,
        last_error,
      });
    }

    for stale in existing
      .values()
      .filter(|record| !seen_paths.contains(&record.path))
    {
      let writer = self.index_writer(&mut writer)?;
      writer.delete_term(Term::from_field_text(self.schema.file_id, &stale.file_id));
      pending_deletes.push(stale.file_id.clone());
    }

    if let Some(mut writer) = writer {
      writer.commit()?;
    }
    for record in pending_upserts {
      self.upsert_file(&record)?;
    }
    for file_id in pending_deletes {
      self.delete_file(&file_id)?;
    }
    files.sort_by(|left, right| left.display_path.cmp(&right.display_path));
    Ok(files)
  }

  pub fn list_files(&self) -> Result<Vec<TubFile>> {
    let mut files = self
      .files_by_path()?
      .into_values()
      .map(TubFile::from)
      .collect::<Vec<_>>();
    files.sort_by(|left, right| left.display_path.cmp(&right.display_path));
    Ok(files)
  }

  pub fn tree_entries(&self, expanded_dirs: &HashSet<PathBuf>) -> Result<Vec<TubTreeNode>> {
    Ok(build_tree_entries(&self.root, self.list_files()?, expanded_dirs))
  }

  pub fn tree_entries_for_files(&self, files: &[TubFile], expanded_dirs: &HashSet<PathBuf>) -> Result<Vec<TubTreeNode>> {
    Ok(build_tree_entries(&self.root, files.to_vec(), expanded_dirs))
  }

  pub fn search_files(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
    if query.trim().is_empty() {
      return Ok(
        self
          .list_files()?
          .into_iter()
          .take(limit)
          .map(SearchHit::from)
          .collect(),
      );
    }
    self.search_tantivy(query, &[SearchUnitKind::File], limit, true)
  }

  pub fn search_content(&self, query: &str, kinds: &[SearchUnitKind], limit: usize) -> Result<Vec<SearchHit>> {
    if query.trim().is_empty() {
      return self.default_content(kinds, limit);
    }
    let kinds = if kinds.is_empty() {
      &[SearchUnitKind::BlockSection, SearchUnitKind::TagSection, SearchUnitKind::Analytic][..]
    } else {
      kinds
    };
    self.search_tantivy(query, kinds, limit, false)
  }

  pub fn default_content(&self, kinds: &[SearchUnitKind], limit: usize) -> Result<Vec<SearchHit>> {
    let kinds = if kinds.is_empty() {
      &[SearchUnitKind::BlockSection, SearchUnitKind::TagSection, SearchUnitKind::Analytic][..]
    } else {
      kinds
    };
    let allowed = kinds.iter().cloned().collect::<HashSet<_>>();
    let mut hits = Vec::with_capacity(limit);

    for file in self.list_files()? {
      if file.kind != FileKind::Db8 || !file.indexed {
        continue;
      }
      for unit in db8_index_units(&file.file_id, &file.path, &file.display_path, &file.file_name)? {
        if allowed.contains(&unit.unit_kind) {
          let mut hit = hit_from_unit(unit);
          self.hydrate_hit_preview(&mut hit)?;
          hits.push(hit);
        }
        if hits.len() >= limit {
          return Ok(hits);
        }
      }
    }

    Ok(hits)
  }

  pub fn start_watcher(&self) -> notify::Result<TubWatcher> {
    let (sender, receiver) = mpsc::channel();
    let root = self.root.clone();
    let mut watcher = notify::recommended_watcher(move |event| {
      let _ = sender.send(event);
    })?;
    watcher.watch(&root, RecursiveMode::Recursive)?;
    Ok(TubWatcher { watcher, receiver })
  }

  fn search_tantivy(&self, query: &str, allowed_kinds: &[SearchUnitKind], limit: usize, filename_only: bool) -> Result<Vec<SearchHit>> {
    register_tokenizers(&self.index);
    let reader = self.index.reader()?;
    let searcher = reader.searcher();
    let fields = if filename_only {
      vec![self.schema.file_name, self.schema.display_path, self.schema.file_name_exact]
    } else {
      vec![
        self.schema.heading,
        self.schema.body,
        self.schema.cite,
        self.schema.file_name,
        self.schema.display_path,
      ]
    };
    let parser = QueryParser::for_index(&self.index, fields);
    let (query, _) = parser.parse_query_lenient(query);
    let top_docs = searcher.search(
      &query,
      &tantivy::collector::TopDocs::with_limit(limit.saturating_mul(8).max(limit)).order_by_score(),
    )?;
    let allowed = allowed_kinds.iter().cloned().collect::<HashSet<_>>();
    let mut hits = Vec::new();

    for (score, address) in top_docs {
      let document = searcher.doc::<TantivyDocument>(address)?;
      let Some(mut hit) = hit_from_document(&self.schema, &document, score) else {
        continue;
      };
      if allowed.contains(&hit.unit_kind) {
        if !filename_only {
          self.hydrate_hit_preview(&mut hit)?;
        }
        hits.push(hit);
      }
      if hits.len() >= limit {
        break;
      }
    }

    Ok(hits)
  }

  fn hydrate_hit_preview(&self, hit: &mut SearchHit) -> Result<()> {
    if !hit.preview_paragraphs.is_empty() {
      return Ok(());
    }
    if hit.paragraph_start_cursor.is_some() && !hit.insert_text.trim().is_empty() {
      hit.preview_paragraphs = vec![preview_paragraph_from_text(&hit.insert_text)];
      return Ok(());
    }
    let Some(start) = hit.paragraph_start else {
      if !hit.insert_text.trim().is_empty() {
        hit.preview_paragraphs = vec![preview_paragraph_from_text(&hit.insert_text)];
      }
      return Ok(());
    };
    let Some(end) = hit.paragraph_end_exclusive else {
      return Ok(());
    };
    if start >= end {
      return Ok(());
    }

    let document = read_db8(&hit.path).with_context(|| format!("reading {}", hit.path.display()))?;
    hit.preview_paragraphs = input_paragraphs_from_document_range(&document, start, end);
    Ok(())
  }

  fn initialize_catalog(&self) -> Result<()> {
    let connection = self.connection()?;
    connection.execute_batch(
      "
      PRAGMA journal_mode = WAL;
      PRAGMA synchronous = NORMAL;
      CREATE TABLE IF NOT EXISTS files (
        file_id TEXT PRIMARY KEY,
        path TEXT NOT NULL UNIQUE,
        display_path TEXT NOT NULL,
        parent_display_path TEXT NOT NULL,
        file_name TEXT NOT NULL,
        kind TEXT NOT NULL,
        size_bytes INTEGER NOT NULL,
        modified_ns INTEGER NOT NULL,
        fingerprint TEXT NOT NULL,
        indexed INTEGER NOT NULL,
        last_error TEXT
      );
      CREATE INDEX IF NOT EXISTS files_display_path_idx ON files(display_path);
      CREATE INDEX IF NOT EXISTS files_parent_idx ON files(parent_display_path);
      ",
    )?;
    Ok(())
  }

  fn connection(&self) -> Result<Connection> {
    Connection::open(&self.catalog_path).with_context(|| format!("opening tub catalog {}", self.catalog_path.display()))
  }

  fn index_writer<'writer>(&self, writer: &'writer mut Option<IndexWriter>) -> Result<&'writer mut IndexWriter> {
    if writer.is_none() {
      *writer = Some(self.index.writer(WRITER_MEMORY_BYTES)?);
    }
    Ok(writer.as_mut().expect("writer initialized"))
  }

  fn files_by_path(&self) -> Result<HashMap<PathBuf, CatalogFileRecord>> {
    let connection = self.connection()?;
    let mut statement = connection.prepare(
      "
      SELECT file_id, path, display_path, parent_display_path, file_name, kind, size_bytes, modified_ns, fingerprint, indexed, last_error
      FROM files
      ",
    )?;
    let rows = statement.query_map([], |row| {
      Ok(CatalogFileRecord {
        file_id: row.get(0)?,
        path: PathBuf::from(row.get::<_, String>(1)?),
        display_path: row.get(2)?,
        parent_display_path: row.get(3)?,
        file_name: row.get(4)?,
        kind: file_kind_from_str(&row.get::<_, String>(5)?).unwrap_or(FileKind::Db8),
        size_bytes: row.get::<_, i64>(6)?.max(0).cast_unsigned(),
        modified_ns: row.get::<_, i64>(7)?.max(0).cast_unsigned(),
        fingerprint: row.get(8)?,
        indexed: row.get::<_, i64>(9)? != 0,
        last_error: row.get(10)?,
      })
    })?;
    let mut records = HashMap::new();
    for row in rows {
      let record = row?;
      records.insert(record.path.clone(), record);
    }
    Ok(records)
  }

  fn upsert_file(&self, record: &CatalogFileRecord) -> Result<()> {
    let connection = self.connection()?;
    connection.execute(
      "
      INSERT INTO files (
        file_id, path, display_path, parent_display_path, file_name, kind, size_bytes, modified_ns, fingerprint, indexed, last_error
      ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
      ON CONFLICT(path) DO UPDATE SET
        file_id = excluded.file_id,
        display_path = excluded.display_path,
        parent_display_path = excluded.parent_display_path,
        file_name = excluded.file_name,
        kind = excluded.kind,
        size_bytes = excluded.size_bytes,
        modified_ns = excluded.modified_ns,
        fingerprint = excluded.fingerprint,
        indexed = excluded.indexed,
        last_error = excluded.last_error
      ",
      params![
        record.file_id.as_str(),
        record.path.to_string_lossy(),
        record.display_path.as_str(),
        record.parent_display_path.as_str(),
        record.file_name.as_str(),
        record.kind.as_str(),
        record.size_bytes.min(i64::MAX as u64) as i64,
        record.modified_ns.min(i64::MAX as u64) as i64,
        record.fingerprint.as_str(),
        i32::from(record.indexed),
        record.last_error.as_deref(),
      ],
    )?;
    Ok(())
  }

  fn delete_file(&self, file_id: &str) -> Result<()> {
    let connection = self.connection()?;
    connection.execute("DELETE FROM files WHERE file_id = ?1", params![file_id])?;
    Ok(())
  }

  #[allow(dead_code, reason = "Point lookup is retained for targeted catalog/debug workflows.")]
  fn file_by_id(&self, file_id: &str) -> Result<Option<TubFile>> {
    let connection = self.connection()?;
    let record = connection
      .query_row(
        "
        SELECT file_id, path, display_path, parent_display_path, file_name, kind, size_bytes, modified_ns, fingerprint, indexed, last_error
        FROM files
        WHERE file_id = ?1
        ",
        params![file_id],
        |row| {
          Ok(CatalogFileRecord {
            file_id: row.get(0)?,
            path: PathBuf::from(row.get::<_, String>(1)?),
            display_path: row.get(2)?,
            parent_display_path: row.get(3)?,
            file_name: row.get(4)?,
            kind: file_kind_from_str(&row.get::<_, String>(5)?).unwrap_or(FileKind::Db8),
            size_bytes: row.get::<_, i64>(6)?.max(0).cast_unsigned(),
            modified_ns: row.get::<_, i64>(7)?.max(0).cast_unsigned(),
            fingerprint: row.get(8)?,
            indexed: row.get::<_, i64>(9)? != 0,
            last_error: row.get(10)?,
          })
        },
      )
      .optional()?;
    Ok(record.map(TubFile::from))
  }
}

pub struct TubWatcher {
  watcher: RecommendedWatcher,
  receiver: Receiver<notify::Result<notify::Event>>,
}

impl TubWatcher {
  #[must_use]
  pub fn drain_events(&self) -> Vec<notify::Result<notify::Event>> {
    let mut events = Vec::new();
    while let Ok(event) = self.receiver.try_recv() {
      events.push(event);
    }
    events
  }

  #[must_use]
  pub fn drain_has_db8_change(&self) -> bool {
    self
      .drain_events()
      .into_iter()
      .any(|event| event.is_ok_and(|event| is_relevant_db8_watch_event(&event)))
  }

  #[must_use]
  pub const fn keepalive(&self) -> &RecommendedWatcher {
    &self.watcher
  }
}

fn is_relevant_db8_watch_event(event: &Event) -> bool {
  if !event.paths.iter().any(|path| is_db8_path(path)) {
    return false;
  }

  matches!(
    event.kind,
    EventKind::Any
      | EventKind::Create(_)
      | EventKind::Remove(_)
      | EventKind::Modify(
        ModifyKind::Any
          | ModifyKind::Data(DataChange::Any | DataChange::Size | DataChange::Content)
          | ModifyKind::Metadata(MetadataKind::WriteTime)
          | ModifyKind::Name(_)
      )
  )
}

fn is_db8_path(path: &Path) -> bool {
  matches!(file_kind_from_path(path), Some(FileKind::Db8))
}

#[derive(Clone, Debug)]
struct TubSchema {
  file_id: Field,
  unit_id: Field,
  unit_kind: Field,
  path: Field,
  display_path: Field,
  file_name: Field,
  file_name_exact: Field,
  heading_path: Field,
  heading: Field,
  cite: Field,
  body: Field,
  insert_text: Field,
  paragraph_start: Field,
  paragraph_end: Field,
  paragraph_start_cursor: Field,
  paragraph_end_cursor: Field,
  size_bytes: Field,
  modified_ns: Field,
}

#[derive(Clone, Debug)]
struct CatalogFileRecord {
  file_id: String,
  path: PathBuf,
  display_path: String,
  parent_display_path: String,
  file_name: String,
  kind: FileKind,
  size_bytes: u64,
  modified_ns: u64,
  fingerprint: String,
  indexed: bool,
  last_error: Option<String>,
}

impl From<CatalogFileRecord> for TubFile {
  fn from(record: CatalogFileRecord) -> Self {
    Self {
      file_id: record.file_id,
      path: record.path,
      display_path: record.display_path,
      parent_display_path: record.parent_display_path,
      file_name: record.file_name,
      kind: record.kind,
      size_bytes: record.size_bytes,
      modified_ns: record.modified_ns,
      indexed: record.indexed,
      last_error: record.last_error,
    }
  }
}

impl From<TubFile> for SearchHit {
  fn from(file: TubFile) -> Self {
    Self {
      file_id: file.file_id.clone(),
      unit_id: format!("{}:file", file.file_id),
      unit_kind: SearchUnitKind::File,
      path: file.path,
      display_path: file.display_path,
      file_name: file.file_name.clone(),
      heading_path: Vec::new(),
      title: file.file_name,
      cite: None,
      snippet: String::new(),
      insert_text: String::new(),
      preview_paragraphs: Vec::new(),
      score: 0.0,
      paragraph_start: None,
      paragraph_end_exclusive: None,
      paragraph_start_cursor: None,
      paragraph_end_cursor: None,
    }
  }
}

#[derive(Debug)]
struct IndexUnit {
  file_id: String,
  unit_id: String,
  unit_kind: SearchUnitKind,
  path: PathBuf,
  display_path: String,
  file_name: String,
  heading_path: Vec<String>,
  heading: String,
  cite: Option<String>,
  body: String,
  insert_text: String,
  paragraph_start: Option<usize>,
  paragraph_end_exclusive: Option<usize>,
  paragraph_start_cursor: Option<Vec<u8>>,
  paragraph_end_cursor: Option<Vec<u8>>,
}

struct FileDocumentInput<'input> {
  file_id: &'input str,
  kind: FileKind,
  path: &'input Path,
  display_path: &'input str,
  file_name: &'input str,
  size_bytes: u64,
  modified_ns: u64,
}

fn build_schema() -> (Schema, TubSchema) {
  let mut builder = Schema::builder();
  let filename_indexing = TextFieldIndexing::default()
    .set_tokenizer(FILENAME_TOKENIZER)
    .set_index_option(IndexRecordOption::WithFreqsAndPositions);
  let filename_options = TextOptions::default()
    .set_indexing_options(filename_indexing)
    .set_stored();

  let file_id = builder.add_text_field("file_id", STRING | STORED);
  let unit_id = builder.add_text_field("unit_id", STRING | STORED);
  let unit_kind = builder.add_text_field("unit_kind", STRING | STORED);
  let path = builder.add_text_field("path", STORED);
  let display_path = builder.add_text_field("display_path", TEXT | STORED);
  let file_name = builder.add_text_field("file_name", filename_options);
  let file_name_exact = builder.add_text_field("file_name_exact", STRING | STORED);
  let heading_path = builder.add_text_field("heading_path", TEXT | STORED);
  let heading = builder.add_text_field("heading", TEXT | STORED);
  let cite = builder.add_text_field("cite", TEXT | STORED);
  let body = builder.add_text_field("body", TEXT | STORED);
  let insert_text = builder.add_text_field("insert_text", STORED);
  let paragraph_start = builder.add_text_field("paragraph_start", STORED);
  let paragraph_end = builder.add_text_field("paragraph_end", STORED);
  let paragraph_start_cursor = builder.add_text_field("paragraph_start_cursor", STORED);
  let paragraph_end_cursor = builder.add_text_field("paragraph_end_cursor", STORED);
  let size_bytes = builder.add_u64_field("size_bytes", STORED);
  let modified_ns = builder.add_u64_field("modified_ns", STORED);
  let schema = builder.build();
  let fields = TubSchema {
    file_id,
    unit_id,
    unit_kind,
    path,
    display_path,
    file_name,
    file_name_exact,
    heading_path,
    heading,
    cite,
    body,
    insert_text,
    paragraph_start,
    paragraph_end,
    paragraph_start_cursor,
    paragraph_end_cursor,
    size_bytes,
    modified_ns,
  };
  (schema, fields)
}

fn register_tokenizers(index: &Index) {
  if let Ok(tokenizer) = NgramTokenizer::new(2, 8, true) {
    index.tokenizers().register(FILENAME_TOKENIZER, tokenizer);
  }
}

fn file_document(schema: &TubSchema, input: FileDocumentInput<'_>) -> TantivyDocument {
  doc!(
    schema.file_id => input.file_id,
    schema.unit_id => format!("{}:file", input.file_id),
    schema.unit_kind => SearchUnitKind::File.as_str(),
    schema.path => input.path.to_string_lossy().to_string(),
    schema.display_path => input.display_path,
    schema.file_name => input.file_name,
    schema.file_name_exact => input.file_name,
    schema.heading_path => "",
    schema.heading => input.file_name,
    schema.cite => input.kind.as_str(),
    schema.body => "",
    schema.insert_text => "",
    schema.paragraph_start => "",
    schema.paragraph_end => "",
    schema.paragraph_start_cursor => "",
    schema.paragraph_end_cursor => "",
    schema.size_bytes => input.size_bytes,
    schema.modified_ns => input.modified_ns,
  )
}

fn unit_document(schema: &TubSchema, unit: &IndexUnit) -> TantivyDocument {
  doc!(
    schema.file_id => unit.file_id.as_str(),
    schema.unit_id => unit.unit_id.as_str(),
    schema.unit_kind => unit.unit_kind.as_str(),
    schema.path => unit.path.to_string_lossy().to_string(),
    schema.display_path => unit.display_path.as_str(),
    schema.file_name => unit.file_name.as_str(),
    schema.file_name_exact => unit.file_name.as_str(),
    schema.heading_path => unit.heading_path.join(" / "),
    schema.heading => unit.heading.as_str(),
    schema.cite => unit.cite.as_deref().unwrap_or(""),
    schema.body => unit.body.as_str(),
    schema.insert_text => unit.insert_text.as_str(),
    schema.paragraph_start => unit.paragraph_start.map(|value| value.to_string()).unwrap_or_default(),
    schema.paragraph_end => unit.paragraph_end_exclusive.map(|value| value.to_string()).unwrap_or_default(),
    schema.paragraph_start_cursor => unit.paragraph_start_cursor.as_deref().map(hex_bytes).unwrap_or_default(),
    schema.paragraph_end_cursor => unit.paragraph_end_cursor.as_deref().map(hex_bytes).unwrap_or_default(),
    schema.size_bytes => unit.insert_text.len() as u64,
    schema.modified_ns => 0_u64,
  )
}

fn hit_from_unit(unit: IndexUnit) -> SearchHit {
  SearchHit {
    file_id: unit.file_id,
    unit_id: unit.unit_id,
    unit_kind: unit.unit_kind,
    path: unit.path,
    display_path: unit.display_path,
    file_name: unit.file_name,
    heading_path: unit.heading_path,
    title: unit.heading,
    cite: unit.cite,
    snippet: preview_text(&unit.body, 360),
    insert_text: unit.insert_text,
    preview_paragraphs: Vec::new(),
    score: 0.0,
    paragraph_start: unit.paragraph_start,
    paragraph_end_exclusive: unit.paragraph_end_exclusive,
    paragraph_start_cursor: unit.paragraph_start_cursor,
    paragraph_end_cursor: unit.paragraph_end_cursor,
  }
}

fn hit_from_document(schema: &TubSchema, document: &TantivyDocument, score: f32) -> Option<SearchHit> {
  let unit_kind = SearchUnitKind::from_str(&stored_text(document, schema.unit_kind)?)?;
  let heading_path = stored_text(document, schema.heading_path)
    .unwrap_or_default()
    .split(" / ")
    .filter(|part| !part.is_empty())
    .map(ToOwned::to_owned)
    .collect::<Vec<_>>();
  Some(SearchHit {
    file_id: stored_text(document, schema.file_id)?,
    unit_id: stored_text(document, schema.unit_id)?,
    unit_kind,
    path: PathBuf::from(stored_text(document, schema.path)?),
    display_path: stored_text(document, schema.display_path)?,
    file_name: stored_text(document, schema.file_name_exact)?,
    heading_path,
    title: stored_text(document, schema.heading).unwrap_or_default(),
    cite: non_empty(stored_text(document, schema.cite).unwrap_or_default()),
    snippet: preview_text(&stored_text(document, schema.body).unwrap_or_default(), 360),
    insert_text: stored_text(document, schema.insert_text).unwrap_or_default(),
    preview_paragraphs: Vec::new(),
    score,
    paragraph_start: stored_text(document, schema.paragraph_start).and_then(|value| value.parse::<usize>().ok()),
    paragraph_end_exclusive: stored_text(document, schema.paragraph_end).and_then(|value| value.parse::<usize>().ok()),
    paragraph_start_cursor: stored_text(document, schema.paragraph_start_cursor).and_then(|value| unhex_bytes(&value)),
    paragraph_end_cursor: stored_text(document, schema.paragraph_end_cursor).and_then(|value| unhex_bytes(&value)),
  })
}

fn stored_text(document: &TantivyDocument, field: Field) -> Option<String> {
  document
    .get_first(field)
    .and_then(|value| value.as_value().as_str())
    .map(ToOwned::to_owned)
}

fn hex_bytes(bytes: &[u8]) -> String {
  let mut out = String::with_capacity(bytes.len() * 2);
  for byte in bytes {
    use std::fmt::Write as _;
    let _ = write!(&mut out, "{byte:02x}");
  }
  out
}

fn unhex_bytes(value: &str) -> Option<Vec<u8>> {
  if value.is_empty() {
    return None;
  }
  let mut bytes = Vec::with_capacity(value.len() / 2);
  let mut chunks = value.as_bytes().chunks_exact(2);
  if !chunks.remainder().is_empty() {
    return None;
  }
  for chunk in &mut chunks {
    let text = std::str::from_utf8(chunk).ok()?;
    bytes.push(u8::from_str_radix(text, 16).ok()?);
  }
  Some(bytes)
}

fn db8_index_units(file_id: &str, path: &Path, display_path: &str, file_name: &str) -> Result<Vec<IndexUnit>> {
  if let Some(units) =
    DocumentPackage::read_cached_search_units(path).with_context(|| format!("reading cached Flowstate search units {}", path.display()))?
  {
    return Ok(
      units
        .iter()
        .filter_map(|unit| package_search_unit(file_id, path, display_path, file_name, unit))
        .collect(),
    );
  }
  let mut package = DocumentPackage::read(path).with_context(|| format!("reading Flowstate package {}", path.display()))?;
  if package.current_search_units().is_empty() {
    let doc = package
      .load_loro_doc()
      .with_context(|| format!("loading Loro document {}", path.display()))?;
    package
      .rebuild_search_units_from_loro(&doc)
      .with_context(|| format!("rebuilding Loro search units {}", path.display()))?;
  }
  Ok(
    package
      .current_search_units()
      .iter()
      .filter_map(|unit| package_search_unit(file_id, path, display_path, file_name, unit))
      .collect(),
  )
}

fn package_search_unit(file_id: &str, path: &Path, display_path: &str, file_name: &str, unit: &SearchUnitChunk) -> Option<IndexUnit> {
  let unit_kind = SearchUnitKind::from_str(&unit.unit_kind)?;
  let body = unit.body.trim().to_string();
  if body.is_empty() {
    return None;
  }
  let heading = if unit.heading.trim().is_empty() {
    first_non_empty_line(&body).unwrap_or_else(|| unit_kind.as_str().to_string())
  } else {
    unit.heading.clone()
  };
  Some(IndexUnit {
    file_id: file_id.to_owned(),
    unit_id: format!("{file_id}:loro:{:032x}", unit.unit_id),
    unit_kind,
    path: path.to_path_buf(),
    display_path: display_path.to_owned(),
    file_name: file_name.to_owned(),
    heading_path: unit.heading_path.clone(),
    heading,
    cite: None,
    body: body.clone(),
    insert_text: if unit.insert_text.is_empty() { body } else { unit.insert_text.clone() },
    paragraph_start: None,
    paragraph_end_exclusive: None,
    paragraph_start_cursor: Some(cursor_for_index(unit)).filter(|cursor| !cursor.is_empty()),
    paragraph_end_cursor: Some(end_cursor_for_index(unit)).filter(|cursor| !cursor.is_empty()),
  })
}

fn cursor_for_index(unit: &SearchUnitChunk) -> Vec<u8> {
  if unit.paragraph_start_cursor.is_empty() {
    unit.unit_start_cursor.clone()
  } else {
    unit.paragraph_start_cursor.clone()
  }
}

fn end_cursor_for_index(unit: &SearchUnitChunk) -> Vec<u8> {
  if unit.paragraph_end_cursor.is_empty() {
    unit.unit_end_cursor.clone()
  } else {
    unit.paragraph_end_cursor.clone()
  }
}

fn preview_paragraph_from_text(text: &str) -> InputParagraph {
  InputParagraph {
    style: flowstate_document::ParagraphStyle::Normal,
    runs: vec![InputRun {
      text: text.to_string(),
      styles: flowstate_document::RunStyles::default(),
    }],
  }
}

fn input_paragraphs_from_document_range(document: &DocumentProjection, start: usize, end: usize) -> Vec<InputParagraph> {
  (start..end.min(document.paragraphs.len()))
    .map(|paragraph_ix| input_paragraph_from_document_range(document, paragraph_ix, 0..paragraph_text_len(&document.paragraphs[paragraph_ix])))
    .filter(|paragraph| paragraph.runs.iter().any(|run| !run.text.is_empty()))
    .collect()
}

fn input_paragraph_from_document_range(document: &DocumentProjection, paragraph_ix: usize, range: std::ops::Range<usize>) -> InputParagraph {
  let paragraph = &document.paragraphs[paragraph_ix];
  let paragraph_range = paragraph_byte_range(document, paragraph_ix);
  let start = range.start.min(paragraph_text_len(paragraph));
  let end = range.end.min(paragraph_text_len(paragraph)).max(start);
  let mut runs = Vec::new();
  let mut offset = 0;
  for run in &paragraph.runs {
    let run_start = offset;
    let run_end = offset + run.len;
    offset = run_end;
    let clipped_start = run_start.max(start);
    let clipped_end = run_end.min(end);
    if clipped_start < clipped_end {
      runs.push(InputRun {
        text: document_text_slice(document, paragraph_range.start + clipped_start..paragraph_range.start + clipped_end),
        styles: run.styles,
      });
    }
  }
  InputParagraph {
    style: paragraph.style,
    runs,
  }
}

fn first_non_empty_line(text: &str) -> Option<String> {
  text
    .lines()
    .map(str::trim)
    .find(|line| !line.is_empty())
    .map(|line| preview_text(line, 120))
}

fn preview_text(text: &str, max_chars: usize) -> String {
  // §perf: build the whitespace-normalized string in a single pass, avoiding the
  // intermediate `Vec<&str>` allocation from `.collect().join(" ")`. Output is
  // identical: `split_whitespace` drops all whitespace runs and we insert a
  // single space between words with no leading/trailing space.
  let mut normalized = String::with_capacity(text.len());
  for word in text.split_whitespace() {
    if !normalized.is_empty() {
      normalized.push(' ');
    }
    normalized.push_str(word);
  }
  if normalized.chars().count() <= max_chars {
    return normalized;
  }
  let mut preview = normalized
    .chars()
    .take(max_chars.saturating_sub(1))
    .collect::<String>();
  preview.push_str("...");
  preview
}

fn non_empty(value: String) -> Option<String> {
  (!value.trim().is_empty()).then_some(value)
}

fn file_kind_from_path(path: &Path) -> Option<FileKind> {
  if is_word_temp_lock_file(path) {
    return None;
  }

  let extension = path.extension()?.to_str()?;
  file_kind_from_str(extension)
}

fn file_kind_from_str(extension: &str) -> Option<FileKind> {
  match extension.to_ascii_lowercase().as_str() {
    "db8" => Some(FileKind::Db8),
    "docx" => Some(FileKind::Docx),
    "fl0" => Some(FileKind::Fl0),
    _ => None,
  }
}

fn is_word_temp_lock_file(path: &Path) -> bool {
  let has_docx_extension = path
    .extension()
    .and_then(|extension| extension.to_str())
    .is_some_and(|extension| extension.eq_ignore_ascii_case("docx"));

  path
    .file_name()
    .and_then(|name| name.to_str())
    .is_some_and(|name| name.starts_with("~$") && has_docx_extension)
}

fn canonicalize_dir(path: &Path) -> Result<PathBuf> {
  path
    .canonicalize()
    .with_context(|| format!("canonicalizing tub root {}", path.display()))
}

fn canonicalize_file(path: &Path) -> Result<PathBuf> {
  path
    .canonicalize()
    .with_context(|| format!("canonicalizing tub file {}", path.display()))
}

fn display_path_for(root: &Path, path: &Path) -> String {
  path
    .strip_prefix(root)
    .unwrap_or(path)
    .to_string_lossy()
    .replace('\\', "/")
}

fn parent_display_path(display_path: &str) -> String {
  Path::new(display_path)
    .parent()
    .map(|parent| parent.to_string_lossy().replace('\\', "/"))
    .unwrap_or_default()
}

fn modified_ns(metadata: &fs::Metadata) -> u64 {
  u64::try_from(
    metadata
      .modified()
      .unwrap_or(SystemTime::UNIX_EPOCH)
      .duration_since(UNIX_EPOCH)
      .unwrap_or_default()
      .as_nanos()
      .min(u128::from(u64::MAX)),
  )
  .expect("nanosecond timestamp is clamped to u64::MAX")
}

fn fingerprint(size_bytes: u64, modified_ns: u64, kind: FileKind, path: &Path) -> Result<String> {
  let mut fingerprint = format!("{size_bytes}:{modified_ns}");
  if kind == FileKind::Db8
    && let Some((frontier, unit_count)) = cached_search_metadata(path)?
  {
    fingerprint.push(':');
    fingerprint.push_str(&frontier);
    fingerprint.push(':');
    fingerprint.push_str(&unit_count.to_string());
  }
  Ok(fingerprint)
}

fn cached_search_metadata(path: &Path) -> Result<Option<(String, usize)>> {
  let Some(units) =
    DocumentPackage::read_cached_search_units(path).with_context(|| format!("reading cached Flowstate search units {}", path.display()))?
  else {
    return Ok(None);
  };
  let frontier = units
    .first()
    .map(|unit| hex_bytes(&unit.frontier))
    .unwrap_or_default();
  Ok(Some((frontier, units.len())))
}

fn stable_file_id(root: &Path, path: &Path) -> String {
  let mut hasher = std::collections::hash_map::DefaultHasher::new();
  display_path_for(root, path).hash(&mut hasher);
  format!("{:016x}", hasher.finish())
}

fn build_tree_entries(root: &Path, files: Vec<TubFile>, expanded_dirs: &HashSet<PathBuf>) -> Vec<TubTreeNode> {
  let mut dirs = BTreeSet::<PathBuf>::new();
  let mut files_by_parent = BTreeMap::<PathBuf, Vec<TubFile>>::new();
  let mut child_dirs = BTreeMap::<PathBuf, BTreeSet<PathBuf>>::new();

  for file in files {
    let relative_parent = PathBuf::from(&file.parent_display_path);
    let mut current = PathBuf::new();
    for component in relative_parent.components() {
      let next = current.join(component.as_os_str());
      dirs.insert(next.clone());
      child_dirs
        .entry(current.clone())
        .or_default()
        .insert(next.clone());
      current = next;
    }
    files_by_parent
      .entry(relative_parent)
      .or_default()
      .push(file);
  }

  for files in files_by_parent.values_mut() {
    files.sort_by(|left, right| left.file_name.cmp(&right.file_name));
  }

  let mut context = TreeEmitContext {
    root,
    dirs: &dirs,
    child_dirs: &child_dirs,
    files_by_parent: &files_by_parent,
    expanded_dirs,
    entries: Vec::new(),
  };
  emit_tree_dir(Path::new(""), 0, &mut context);
  context.entries
}

struct TreeEmitContext<'tree> {
  root: &'tree Path,
  dirs: &'tree BTreeSet<PathBuf>,
  child_dirs: &'tree BTreeMap<PathBuf, BTreeSet<PathBuf>>,
  files_by_parent: &'tree BTreeMap<PathBuf, Vec<TubFile>>,
  expanded_dirs: &'tree HashSet<PathBuf>,
  entries: Vec<TubTreeNode>,
}

fn emit_tree_dir(relative_dir: &Path, depth: usize, context: &mut TreeEmitContext<'_>) {
  if depth > 0 {
    let absolute = context.root.join(relative_dir);
    let expanded = context.expanded_dirs.contains(&absolute);
    context.entries.push(TubTreeNode {
      path: absolute,
      display_path: relative_dir.to_string_lossy().replace('\\', "/"),
      name: relative_dir
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default(),
      is_dir: true,
      depth: depth - 1,
      expanded,
      file_kind: None,
    });
    if !expanded {
      return;
    }
  }

  let children = context
    .child_dirs
    .get(relative_dir)
    .cloned()
    .unwrap_or_default();
  for child in children {
    if context.dirs.contains(&child) {
      emit_tree_dir(&child, depth + 1, context);
    }
  }

  if let Some(files) = context.files_by_parent.get(relative_dir).cloned() {
    for file in files {
      context.entries.push(TubTreeNode {
        path: file.path.clone(),
        display_path: file.display_path.clone(),
        name: file.file_name.clone(),
        is_dir: false,
        depth,
        expanded: false,
        file_kind: Some(file.kind),
      });
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn unknown_search_unit_kind_is_preserved() {
    let kind = SearchUnitKind::from_str("mystery_kind").expect("unknown kind should be preserved");
    assert_eq!(kind.as_str(), "mystery_kind");
    assert!(matches!(kind, SearchUnitKind::Unknown(value) if value == "mystery_kind"));
  }
}
