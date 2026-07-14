use std::{
  fs,
  path::{Component, Path, PathBuf},
  sync::Mutex,
};

use fff_search::{FFFMode, FilePickerOptions, FuzzySearchOptions, PaginationArgs, QueryParser, file_picker::FilePicker};

const SUPPORTED_DOCUMENT_EXTENSIONS: [&str; 4] = ["db8", "docx", "pdf", "fl0"];
const EXTENSION_CONSTRAINTS: &str = "*.db8 *.docx *.pdf *.fl0";
const SEARCH_OVERSAMPLE_FACTOR: usize = 16;

#[derive(Clone, Debug)]
pub struct FileSearchHit {
  pub path: PathBuf,
}

pub struct DocumentFileSearch {
  root: PathBuf,
  picker: Mutex<FilePicker>,
  supplemental_files: Vec<DocumentFileEntry>,
  indexed_file_count: usize,
}

struct DocumentFileEntry {
  path: PathBuf,
  file_name_lower: String,
  full_path_lower: String,
}

#[hotpath::measure_all]
impl DocumentFileEntry {
  fn new(path: PathBuf) -> Self {
    let file_name_lower = path
      .file_name()
      .map(|name| name.to_string_lossy().to_ascii_lowercase())
      .unwrap_or_default();
    let full_path_lower = path.to_string_lossy().to_ascii_lowercase();
    Self {
      path,
      file_name_lower,
      full_path_lower,
    }
  }
}

#[hotpath::measure_all]
impl DocumentFileSearch {
  pub fn new(root: PathBuf) -> anyhow::Result<Self> {
    let root = normalize_search_root(root)?;
    let mut picker = FilePicker::new(FilePickerOptions {
      base_path: root.to_string_lossy().to_string(),
      enable_mmap_cache: false,
      enable_content_indexing: false,
      mode: FFFMode::Ai,
      watch: false,
      ..Default::default()
    })?;
    picker.collect_files()?;
    let fff_document_paths = picker
      .get_files()
      .iter()
      .filter_map(|file| {
        let path = file.absolute_path(&picker, &root);
        is_visible_supported_document_path(&path, &root).then_some(path)
      })
      .collect::<Vec<_>>();
    let supplemental_files = collect_supplemental_document_files(&root, &fff_document_paths);
    let indexed_file_count = fff_document_paths.len() + supplemental_files.len();
    Ok(Self {
      root,
      picker: Mutex::new(picker),
      supplemental_files,
      indexed_file_count,
    })
  }

  pub fn root(&self) -> &Path {
    &self.root
  }

  pub fn indexed_file_count(&self) -> usize {
    self.indexed_file_count
  }

  pub fn search(&self, query: &str, limit: usize) -> Vec<FileSearchHit> {
    let Ok(picker) = self.picker.lock() else {
      return Vec::new();
    };
    search_document_files(&picker, &self.root, &self.supplemental_files, query, limit)
  }
}

#[hotpath::measure]
pub fn default_global_search_root() -> PathBuf {
  std::env::var_os("HOME")
    .map(PathBuf::from)
    .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

#[hotpath::measure]
fn normalize_search_root(root: PathBuf) -> anyhow::Result<PathBuf> {
  let root = root.canonicalize().unwrap_or(root);
  if !root.exists() {
    anyhow::bail!("search root does not exist: {}", root.display());
  }
  if root.parent().is_none() {
    anyhow::bail!("refusing to search the filesystem root; choose a narrower root");
  }
  Ok(root)
}

#[hotpath::measure]
fn search_document_files(
  picker: &FilePicker,
  root: &Path,
  supplemental_files: &[DocumentFileEntry],
  typed_query: &str,
  limit: usize,
) -> Vec<FileSearchHit> {
  if limit == 0 {
    return Vec::new();
  }

  let query = typed_query.trim();
  if query.is_empty() {
    let mut hits = picker
      .get_files()
      .iter()
      .filter_map(|file| supported_hit_from_fff_file(picker, root, file))
      .chain(
        supplemental_files
          .iter()
          .map(|entry| FileSearchHit { path: entry.path.clone() }),
      )
      .collect::<Vec<_>>();
    hits.sort_by(|a, b| a.path.cmp(&b.path));
    hits.truncate(limit);
    return hits;
  }

  let parser = QueryParser::default();
  let constrained_query = format!("{query} {EXTENSION_CONSTRAINTS}");
  let parsed = parser.parse(&constrained_query);
  let result = picker.fuzzy_search(
    &parsed,
    None,
    FuzzySearchOptions {
      max_threads: 0,
      current_file: None,
      project_path: Some(root),
      pagination: PaginationArgs {
        offset: 0,
        limit: limit.saturating_mul(SEARCH_OVERSAMPLE_FACTOR).max(limit),
      },
      ..Default::default()
    },
  );

  let mut hits = result
    .items
    .iter()
    .filter_map(|file| supported_hit_from_fff_file(picker, root, file))
    .collect::<Vec<_>>();

  // §perf: dedup against borrowed hit paths instead of cloning each PathBuf into the set.
  let existing = hits
    .iter()
    .map(|hit| hit.path.as_path())
    .collect::<std::collections::HashSet<&std::path::Path>>();
  let mut supplemental_hits =
    search_supplemental_document_files(supplemental_files, query, limit.saturating_mul(SEARCH_OVERSAMPLE_FACTOR).max(limit))
      .into_iter()
      .filter(|hit| !existing.contains(hit.path.as_path()))
      .collect::<Vec<_>>();
  hits.append(&mut supplemental_hits);
  hits.truncate(limit);
  hits
}

#[hotpath::measure]
fn supported_hit_from_fff_file(picker: &FilePicker, root: &Path, file: &fff_search::FileItem) -> Option<FileSearchHit> {
  let path = file.absolute_path(picker, root);
  is_visible_supported_document_path(&path, root).then_some(FileSearchHit { path })
}

#[hotpath::measure]
fn is_visible_supported_document_path(path: &Path, root: &Path) -> bool {
  is_supported_document_path(path) && !has_hidden_component_under_root(path, root)
}

#[hotpath::measure]
fn collect_supplemental_document_files(root: &Path, fff_document_paths: &[PathBuf]) -> Vec<DocumentFileEntry> {
  let fff_document_paths = fff_document_paths
    .iter()
    .collect::<std::collections::HashSet<_>>();
  let mut files = Vec::new();
  collect_visible_document_files(root, root, &fff_document_paths, &mut files);
  let mut files = files
    .into_iter()
    .map(DocumentFileEntry::new)
    .collect::<Vec<_>>();
  files.sort_by(|a, b| a.full_path_lower.cmp(&b.full_path_lower));
  files
}

#[hotpath::measure]
fn collect_visible_document_files(root: &Path, path: &Path, indexed_paths: &std::collections::HashSet<&PathBuf>, files: &mut Vec<PathBuf>) {
  let Ok(entries) = fs::read_dir(path) else {
    return;
  };

  for entry in entries.flatten() {
    let path = entry.path();
    let Ok(file_type) = entry.file_type() else {
      continue;
    };

    if file_type.is_dir() {
      if !has_hidden_component_under_root(&path, root) {
        collect_visible_document_files(root, &path, indexed_paths, files);
      }
    } else if file_type.is_file() && is_visible_supported_document_path(&path, root) && !indexed_paths.contains(&path) {
      files.push(path);
    }
  }
}

#[hotpath::measure]
fn search_supplemental_document_files(files: &[DocumentFileEntry], typed_query: &str, limit: usize) -> Vec<FileSearchHit> {
  let query = typed_query.trim().to_ascii_lowercase();
  if query.is_empty() {
    return files
      .iter()
      .take(limit)
      .map(|entry| FileSearchHit { path: entry.path.clone() })
      .collect();
  }

  let mut scored = files
    .iter()
    .filter_map(|entry| match_path(entry, &query).map(|score| (score, entry)))
    .collect::<Vec<_>>();
  scored.sort_by(|(a_score, a_entry), (b_score, b_entry)| {
    a_score
      .cmp(b_score)
      .then_with(|| a_entry.full_path_lower.cmp(&b_entry.full_path_lower))
  });

  scored
    .into_iter()
    .take(limit)
    .map(|(_, entry)| FileSearchHit { path: entry.path.clone() })
    .collect()
}

#[hotpath::measure]
fn match_path(entry: &DocumentFileEntry, query: &str) -> Option<usize> {
  if let Some(index) = entry.file_name_lower.find(query) {
    return Some(index);
  }

  if let Some(index) = entry.full_path_lower.find(query) {
    return Some(entry.file_name_lower.len() + index);
  }

  fuzzy_subsequence_score(&entry.file_name_lower, query)
    .or_else(|| fuzzy_subsequence_score(&entry.full_path_lower, query).map(|score| entry.file_name_lower.len() + score))
}

#[hotpath::measure]
fn fuzzy_subsequence_score(haystack: &str, needle: &str) -> Option<usize> {
  let mut score = 0;
  let mut haystack_chars = haystack.char_indices();

  for needle_char in needle.chars() {
    let (index, _) = haystack_chars.find(|(_, haystack_char)| *haystack_char == needle_char)?;
    score += index;
  }

  Some(score)
}

#[hotpath::measure]
fn has_hidden_component_under_root(path: &Path, root: &Path) -> bool {
  path
    .strip_prefix(root)
    .unwrap_or(path)
    .components()
    .any(|component| match component {
      Component::Normal(name) => name.to_str().is_some_and(|name| name.starts_with('.')),
      _ => false,
    })
}

#[hotpath::measure]
fn is_supported_document_path(path: &Path) -> bool {
  if is_word_temp_lock_file(path) {
    return false;
  }

  path
    .extension()
    .and_then(|extension| extension.to_str())
    // §perf: compare case-insensitively without allocating a lowercased String.
    .is_some_and(|extension| SUPPORTED_DOCUMENT_EXTENSIONS.iter().any(|e| extension.eq_ignore_ascii_case(e)))
}

#[hotpath::measure]
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

#[cfg(test)]
mod tests {
  use super::*;
  use tempfile::tempdir;

  fn write_file(root: &Path, relative: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
      std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, b"test").unwrap();
  }

  #[test]
  fn global_document_search_uses_fff_with_existing_document_filters() {
    let dir = tempdir().unwrap();
    write_file(dir.path(), "alpha.db8");
    write_file(dir.path(), "beta.docx");
    write_file(dir.path(), "gamma.pdf");
    write_file(dir.path(), "delta.fl0");
    write_file(dir.path(), "notes.txt");
    write_file(dir.path(), ".hidden/secret.db8");
    write_file(dir.path(), "~$locked.docx");

    let search = DocumentFileSearch::new(dir.path().to_path_buf()).unwrap();
    assert_eq!(search.indexed_file_count(), 4);

    let hits = search.search("", 10);
    let names = hits
      .iter()
      .filter_map(|hit| hit.path.file_name().and_then(|name| name.to_str()))
      .collect::<Vec<_>>();

    assert_eq!(names, vec!["alpha.db8", "beta.docx", "delta.fl0", "gamma.pdf"]);
  }

  #[test]
  fn global_document_search_returns_fff_matches_and_supplemental_documents() {
    let dir = tempdir().unwrap();
    write_file(dir.path(), "cases/abolition-k.docx");
    write_file(dir.path(), "cases/analytics.db8");
    write_file(dir.path(), "cases/ballot.txt");

    let search = DocumentFileSearch::new(dir.path().to_path_buf()).unwrap();
    let fff_hits = search.search("analy", 10);
    let fff_names = fff_hits
      .iter()
      .filter_map(|hit| hit.path.file_name().and_then(|name| name.to_str()))
      .collect::<Vec<_>>();
    assert_eq!(fff_names, vec!["analytics.db8"]);

    let supplemental_hits = search.search("abolition", 10);
    let supplemental_names = supplemental_hits
      .iter()
      .filter_map(|hit| hit.path.file_name().and_then(|name| name.to_str()))
      .collect::<Vec<_>>();
    assert_eq!(supplemental_names, vec!["abolition-k.docx"]);
  }
}
