use std::{
  collections::hash_map::DefaultHasher,
  fs,
  hash::{Hash, Hasher},
  io::{self, Cursor, Read, Write},
  path::{Path, PathBuf},
  time::Instant,
};

use crop::Rope;
use tempfile::NamedTempFile;

use super::*;

// DB8 is our on-disk document format: a magic header, a version, the raw
// UTF-8 text blob, then per-paragraph run metadata. Keeping the format
// length-prefixed makes the reader resilient against trailing junk.
const DB8_MAGIC: &[u8; 4] = b"DB8\0";
const DB8_VERSION: u32 = 1;

pub fn load_or_create_document(path: impl AsRef<Path>) -> io::Result<Document> {
  let path = path.as_ref();
  match read_db8(path) {
    Ok(document) => Ok(document),
    Err(error) if error.kind() == io::ErrorKind::NotFound => {
      let document = demo_document();
      write_db8(path, &document)?;
      Ok(document)
    },
    Err(error) => Err(error),
  }
}

pub fn read_db8(path: impl AsRef<Path>) -> io::Result<Document> {
  let timing = Instant::now();
  let bytes = fs::read(path)?;
  let mut cursor = Cursor::new(bytes.as_slice());
  let mut magic = [0; 4];
  cursor.read_exact(&mut magic)?;
  if &magic != DB8_MAGIC {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid DB8 magic"));
  }
  let version = read_u32(&mut cursor)?;
  if version != DB8_VERSION {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "unsupported DB8 version"));
  }

  let text_len = read_u64(&mut cursor)? as usize;
  let mut text_bytes = vec![0; text_len];
  cursor.read_exact(&mut text_bytes)?;
  let text = String::from_utf8(text_bytes).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "DB8 text is not UTF-8"))?;

  let paragraph_count = read_u64(&mut cursor)? as usize;
  let mut paragraphs = Vec::with_capacity(paragraph_count);
  for _ in 0..paragraph_count {
    let style = decode_paragraph_style(read_u8(&mut cursor)?)?;
    let start = read_u64(&mut cursor)? as usize;
    let end = read_u64(&mut cursor)? as usize;
    let run_count = read_u64(&mut cursor)? as usize;
    let mut runs = Vec::with_capacity(run_count);
    for _ in 0..run_count {
      let len = read_u64(&mut cursor)? as usize;
      let styles = decode_run_styles(read_u8(&mut cursor)?)?;
      runs.push(TextRun { len, styles });
    }
    paragraphs.push(Paragraph {
      style,
      byte_range: start..end,
      runs: merge_adjacent_runs(runs),
      version: 0,
    });
  }

  let offset_index = ParagraphOffsetIndex::new(&paragraphs);
  let document = Document {
    text: Rope::from(text),
    paragraphs,
    offset_index,
    theme: DocumentTheme::default(),
  };
  validate_document(&document)?;
  log_timing(
    "db8 read",
    timing,
    format!("bytes={} paragraphs={}", document.text.byte_len(), document.paragraphs.len()),
  );
  Ok(document)
}

pub fn write_db8(path: impl AsRef<Path>, document: &Document) -> io::Result<()> {
  let path = path.as_ref();
  if let Some(parent) = path.parent() {
    fs::create_dir_all(parent)?;
  }
  validate_document(document)?;
  let bytes = serialize_db8(document)?;
  write_bytes_atomic(path, &bytes)
}

fn serialize_db8(document: &Document) -> io::Result<Vec<u8>> {
  let mut bytes = Vec::new();
  bytes.extend_from_slice(DB8_MAGIC);
  bytes.extend_from_slice(&DB8_VERSION.to_le_bytes());
  let text = document_text_slice(document, 0..document.text.byte_len());
  write_u64(&mut bytes, text.len() as u64);
  bytes.extend_from_slice(text.as_bytes());
  write_u64(&mut bytes, document.paragraphs.len() as u64);
  for (paragraph_ix, paragraph) in document.paragraphs.iter().enumerate() {
    let range = paragraph_byte_range(document, paragraph_ix);
    bytes.push(encode_paragraph_style(paragraph.style));
    write_u64(&mut bytes, range.start as u64);
    write_u64(&mut bytes, range.end as u64);
    write_u64(&mut bytes, paragraph.runs.len() as u64);
    for run in &paragraph.runs {
      write_u64(&mut bytes, run.len as u64);
      bytes.push(encode_run_styles(run.styles));
    }
  }
  Ok(bytes)
}

fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
  let parent = path.parent().unwrap_or_else(|| Path::new("."));
  fs::create_dir_all(parent)?;
  let mut temp = NamedTempFile::new_in(parent)?;
  temp.write_all(bytes)?;
  temp.as_file_mut().sync_all()?;
  temp.persist(path).map(|_| ()).map_err(|error| error.error)
}

pub(super) fn recovery_path_for_document(path: &PathBuf) -> PathBuf {
  let mut recovery_path = path.clone();
  let file_name = path
    .file_name()
    .and_then(|name| name.to_str())
    .map(|name| format!("{name}.recovery"))
    .unwrap_or_else(|| "untitled.db8.recovery".to_string());
  recovery_path.set_file_name(file_name);
  recovery_path
}

#[allow(dead_code)]
fn document_fingerprint(document: &Document) -> u64 {
  let mut hasher = DefaultHasher::new();
  document_text_slice(document, 0..document.text.byte_len()).hash(&mut hasher);
  for (paragraph_ix, paragraph) in document.paragraphs.iter().enumerate() {
    let range = paragraph_byte_range(document, paragraph_ix);
    paragraph.style.hash(&mut hasher);
    range.start.hash(&mut hasher);
    range.end.hash(&mut hasher);
    paragraph.runs.hash(&mut hasher);
  }
  hasher.finish()
}

fn validate_document(document: &Document) -> io::Result<()> {
  let text_len = document.text.byte_len();
  if document.paragraphs.is_empty() {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "DB8 document has no paragraphs"));
  }
  for (ix, paragraph) in document.paragraphs.iter().enumerate() {
    let range = paragraph_byte_range(document, ix);
    if range.start > range.end || range.end > text_len {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "paragraph range is outside document text"));
    }
    if ix == 0 && range.start != 0 {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "first paragraph does not start at byte 0"));
    }
    if paragraph_runs_len(paragraph) != paragraph_text_len(paragraph) {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "paragraph run lengths do not match paragraph text",
      ));
    }
    if ix > 0 {
      let previous_range = paragraph_byte_range(document, ix - 1);
      if previous_range.end + 1 != range.start || document.text.byte(previous_range.end) != b'\n' {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "paragraph ranges are not newline separated"));
      }
    }
  }
  if document
    .paragraphs
    .last()
    .is_some_and(|_| paragraph_byte_range(document, document.paragraphs.len() - 1).end != text_len)
  {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "last paragraph does not end at text length"));
  }
  Ok(())
}

fn read_u8(cursor: &mut Cursor<&[u8]>) -> io::Result<u8> {
  let mut bytes = [0; 1];
  cursor.read_exact(&mut bytes)?;
  Ok(bytes[0])
}

fn read_u32(cursor: &mut Cursor<&[u8]>) -> io::Result<u32> {
  let mut bytes = [0; 4];
  cursor.read_exact(&mut bytes)?;
  Ok(u32::from_le_bytes(bytes))
}

fn read_u64(cursor: &mut Cursor<&[u8]>) -> io::Result<u64> {
  let mut bytes = [0; 8];
  cursor.read_exact(&mut bytes)?;
  Ok(u64::from_le_bytes(bytes))
}

fn write_u64(bytes: &mut Vec<u8>, value: u64) {
  bytes.extend_from_slice(&value.to_le_bytes());
}

fn encode_paragraph_style(style: ParagraphStyle) -> u8 {
  match style {
    ParagraphStyle::Pocket => 0,
    ParagraphStyle::Hat => 1,
    ParagraphStyle::Block => 2,
    ParagraphStyle::Tag => 3,
    ParagraphStyle::Analytic => 4,
    ParagraphStyle::Normal => 5,
    ParagraphStyle::Undertag => 6,
  }
}

fn decode_paragraph_style(value: u8) -> io::Result<ParagraphStyle> {
  match value {
    0 => Ok(ParagraphStyle::Pocket),
    1 => Ok(ParagraphStyle::Hat),
    2 => Ok(ParagraphStyle::Block),
    3 => Ok(ParagraphStyle::Tag),
    4 => Ok(ParagraphStyle::Analytic),
    5 => Ok(ParagraphStyle::Normal),
    6 => Ok(ParagraphStyle::Undertag),
    _ => Err(io::Error::new(io::ErrorKind::InvalidData, "invalid paragraph style")),
  }
}

fn encode_run_styles(styles: RunStyles) -> u8 {
  let mut bits = 0;
  if styles.cite {
    bits |= 1 << 0;
  }
  if styles.direct_underline {
    bits |= 1 << 1;
  }
  if styles.emphasis {
    bits |= 1 << 2;
  }
  if styles.style_underline {
    bits |= 1 << 3;
  }
  bits
    | match styles.highlight {
      None => 0,
      Some(HighlightStyle::Spoken) => 1 << 4,
      Some(HighlightStyle::Insert) => 2 << 4,
      Some(HighlightStyle::Alternative) => 3 << 4,
    }
}

fn decode_run_styles(bits: u8) -> io::Result<RunStyles> {
  let highlight = match (bits >> 4) & 0b11 {
    0 => None,
    1 => Some(HighlightStyle::Spoken),
    2 => Some(HighlightStyle::Insert),
    3 => Some(HighlightStyle::Alternative),
    _ => unreachable!(),
  };
  if bits & 0b1100_0000 != 0 {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid run style bits"));
  }
  Ok(RunStyles {
    cite: bits & (1 << 0) != 0,
    direct_underline: bits & (1 << 1) != 0,
    emphasis: bits & (1 << 2) != 0,
    style_underline: bits & (1 << 3) != 0,
    highlight,
  })
}
