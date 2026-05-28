use std::{
  fs,
  path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct FileSearchHit {
  pub path: PathBuf,
}

pub struct DocumentFileSearch {
  root: PathBuf,
  files: Vec<DocumentFileEntry>,
}

struct DocumentFileEntry {
  path: PathBuf,
  file_name_lower: String,
  full_path_lower: String,
}

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

impl DocumentFileSearch {
  pub fn new(root: PathBuf) -> anyhow::Result<Self> {
    let root = normalize_search_root(root)?;
    let mut files = Vec::new();
    collect_document_files(&root, &mut files);
    let mut files = files.into_iter().map(DocumentFileEntry::new).collect::<Vec<_>>();
    files.sort_by(|a, b| a.full_path_lower.cmp(&b.full_path_lower));
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

fn search_document_files(files: &[DocumentFileEntry], typed_query: &str, limit: usize) -> Vec<FileSearchHit> {
  let query = typed_query.trim().to_ascii_lowercase();
  if query.is_empty() {
    return files
      .iter()
      .take(limit)
      .map(|entry| FileSearchHit {
        path: entry.path.clone(),
      })
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
    .map(|(_, entry)| FileSearchHit {
      path: entry.path.clone(),
    })
    .collect()
}

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

fn fuzzy_subsequence_score(haystack: &str, needle: &str) -> Option<usize> {
  let mut score = 0;
  let mut haystack_chars = haystack.char_indices();

  for needle_char in needle.chars() {
    let (index, _) = haystack_chars.find(|(_, haystack_char)| *haystack_char == needle_char)?;
    score += index;
  }

  Some(score)
}

fn is_supported_document_path(path: &Path) -> bool {
  path
    .extension()
    .and_then(|extension| extension.to_str())
    .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "db8" | "docx" | "fl0"))
}
