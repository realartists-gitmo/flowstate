use std::{
  cmp::Ordering,
  fs,
  path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct FileSearchHit {
  pub path: PathBuf,
}

pub struct DocumentFileSearch {
  root: PathBuf,
  files: Vec<PathBuf>,
}

impl DocumentFileSearch {
  pub fn new(root: PathBuf) -> anyhow::Result<Self> {
    let root = normalize_search_root(root)?;
    let mut files = Vec::new();
    collect_document_files(&root, &mut files);
    files.sort_by(|a, b| compare_paths_for_display(a, b));
    Ok(Self { root, files })
  }

  pub fn root(&self) -> &Path {
    &self.root
  }

  pub fn indexed_file_count(&self) -> usize {
    self.files.len()
  }

  pub fn search(&self, query: &str, limit: usize) -> Vec<FileSearchHit> {
    search_document_files(&self.files, query, limit)
  }
}

pub fn default_global_search_root() -> PathBuf {
  std::env::var_os("HOME")
    .map(PathBuf::from)
    .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

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

fn collect_document_files(root: &Path, files: &mut Vec<PathBuf>) {
  let Ok(entries) = fs::read_dir(root) else {
    return;
  };

  for entry in entries.flatten() {
    let path = entry.path();
    let Ok(file_type) = entry.file_type() else {
      continue;
    };

    if file_type.is_dir() {
      if should_descend_into(&path) {
        collect_document_files(&path, files);
      }
    } else if file_type.is_file() && is_supported_document_path(&path) {
      files.push(path);
    }
  }
}

fn should_descend_into(path: &Path) -> bool {
  let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
    return false;
  };

  !name.starts_with('.')
}

fn search_document_files(files: &[PathBuf], typed_query: &str, limit: usize) -> Vec<FileSearchHit> {
  let query = typed_query.trim().to_ascii_lowercase();
  if query.is_empty() {
    return files
      .iter()
      .take(limit)
      .cloned()
      .map(|path| FileSearchHit { path })
      .collect();
  }

  let mut scored = files
    .iter()
    .filter_map(|path| match_path(path, &query).map(|score| (score, path)))
    .collect::<Vec<_>>();
  scored.sort_by(|(a_score, a_path), (b_score, b_path)| {
    a_score
      .cmp(b_score)
      .then_with(|| compare_paths_for_display(a_path, b_path))
  });

  scored
    .into_iter()
    .take(limit)
    .map(|(_, path)| FileSearchHit { path: path.clone() })
    .collect()
}

fn match_path(path: &Path, query: &str) -> Option<usize> {
  let file_name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
  if let Some(index) = file_name.find(query) {
    return Some(index);
  }

  let full_path = path.to_string_lossy().to_ascii_lowercase();
  if let Some(index) = full_path.find(query) {
    return Some(file_name.len() + index);
  }

  fuzzy_subsequence_score(&file_name, query).or_else(|| fuzzy_subsequence_score(&full_path, query).map(|score| file_name.len() + score))
}

fn fuzzy_subsequence_score(haystack: &str, needle: &str) -> Option<usize> {
  let mut score = 0;
  let mut haystack_chars = haystack.char_indices();

  for needle_char in needle.chars() {
    let (index, _) = haystack_chars.find(|(_, haystack_char)| *haystack_char == needle_char)?;
    score += index;
  }

  Some(score)
}

fn compare_paths_for_display(a: &Path, b: &Path) -> Ordering {
  a.to_string_lossy().cmp(&b.to_string_lossy())
}

fn is_supported_document_path(path: &Path) -> bool {
  path
    .extension()
    .and_then(|extension| extension.to_str())
    .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "db8" | "docx"))
}
