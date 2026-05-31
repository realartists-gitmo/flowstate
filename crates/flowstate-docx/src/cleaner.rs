use std::{fs, io::Cursor, path::Path};

use rdocx_opc::OpcPackage;

pub const CLEANING_RULES: &[CleanAction] = &[
  CleanAction::ReadWithRdocx,
  CleanAction::NormalizeUnsupportedFormattingValues,
  CleanAction::RecognizeKnownParagraphAndRunStyles,
  CleanAction::ResolveRunProperties,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CleanAction {
  ReadWithRdocx,
  NormalizeUnsupportedFormattingValues,
  RecognizeKnownParagraphAndRunStyles,
  ResolveRunProperties,
}

#[derive(Clone, Debug)]
pub struct CleanedDocx {
  pub bytes: Vec<u8>,
  pub main_document_xml: Option<Vec<u8>>,
  pub report: DocxCleanReport,
}

#[derive(Clone, Debug)]
pub struct DocxCleanReport {
  pub stats: DocxCleanStats,
  pub actions: &'static [CleanAction],
}

#[derive(Default, Clone, Copy, Debug, Eq, PartialEq)]
pub struct DocxCleanStats {
  pub underline_values_normalized: usize,
  pub highlight_values_normalized: usize,
  pub border_values_normalized: usize,
  pub justification_values_normalized: usize,
  pub tab_values_normalized: usize,
  pub section_values_normalized: usize,
  pub style_type_values_normalized: usize,
  pub styles_normalized: usize,
  pub styles_removed: usize,
  pub paragraphs_restyled: usize,
  pub runs_restyled: usize,
  pub hyperlinks_flattened: usize,
}

#[hotpath::measure]
pub fn clean_docx_path(path: impl AsRef<Path>) -> std::io::Result<CleanedDocx> {
  clean_docx_vec(fs::read(path)?)
}

#[hotpath::measure]
pub fn clean_docx_bytes(bytes: &[u8]) -> std::io::Result<CleanedDocx> {
  clean_docx_vec(bytes.to_vec())
}

#[hotpath::measure]
fn clean_docx_vec(bytes: Vec<u8>) -> std::io::Result<CleanedDocx> {
  let (bytes, main_document_xml, stats) = normalize_docx_formatting_values(bytes)?;
  Ok(CleanedDocx {
    bytes,
    main_document_xml,
    report: DocxCleanReport {
      stats,
      actions: CLEANING_RULES,
    },
  })
}

#[hotpath::measure]
fn normalize_docx_formatting_values(bytes: Vec<u8>) -> std::io::Result<(Vec<u8>, Option<Vec<u8>>, DocxCleanStats)> {
  let mut package = match OpcPackage::from_reader(Cursor::new(bytes.as_slice())) {
    Ok(package) => package,
    Err(_) => return Ok((bytes, None, DocxCleanStats::default())),
  };
  let main_document_part = package.main_document_part();
  let mut stats = DocxCleanStats::default();

  for (part_name, part) in &mut package.parts {
    if !part_might_contain_word_xml(part_name, part) {
      continue;
    }
    let Ok(xml) = std::str::from_utf8(part) else {
      continue;
    };
    let (normalized, part_stats) = normalize_formatting_values_in_xml(xml);
    if let Some(normalized) = normalized {
      *part = normalized.into_bytes();
      stats.merge(part_stats);
    }
  }

  let main_document_xml = main_document_part
    .as_deref()
    .and_then(|part_name| package.get_part(part_name))
    .map(<[u8]>::to_vec);

  if !stats.has_changes() {
    return Ok((bytes, main_document_xml, stats));
  }

  let mut output = Cursor::new(Vec::new());
  package
    .write_to(&mut output)
    .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
  Ok((output.into_inner(), main_document_xml, stats))
}

#[hotpath::measure]
fn part_might_contain_word_xml(part_name: &str, part: &[u8]) -> bool {
  part_name.starts_with("/word/")
    && std::path::Path::new(part_name)
      .extension()
      .is_some_and(|extension| extension.eq_ignore_ascii_case("xml"))
    && (part.starts_with(b"<?xml") || contains_bytes(part, b"<w:") || contains_bytes(part, b"<u "))
}

#[hotpath::measure]
fn normalize_formatting_values_in_xml(xml: &str) -> (Option<String>, DocxCleanStats) {
  let mut normalized = None::<String>;
  let mut cursor = 0_usize;
  let mut flushed_cursor = 0_usize;
  let mut stats = DocxCleanStats::default();

  while let Some(relative_start) = xml[cursor..].find('<') {
    let tag_start = cursor + relative_start;
    let Some(relative_end) = xml[tag_start..].find('>') else {
      break;
    };
    let tag_end = tag_start + relative_end + 1;
    let tag = &xml[tag_start..tag_end];
    let (tag, tag_stats) = normalize_formatting_tag(tag);
    if tag_stats.has_changes() {
      let normalized = normalized.get_or_insert_with(|| String::with_capacity(xml.len()));
      normalized.push_str(&xml[flushed_cursor..tag_start]);
      normalized.push_str(tag.as_deref().unwrap_or_else(|| &xml[tag_start..tag_end]));
      flushed_cursor = tag_end;
      stats.merge(tag_stats);
    }
    cursor = tag_end;
  }

  if let Some(normalized) = normalized.as_mut() {
    normalized.push_str(&xml[flushed_cursor..]);
  }
  (normalized, stats)
}

#[hotpath::measure]
fn normalize_formatting_tag(tag: &str) -> (Option<String>, DocxCleanStats) {
  let Some(name) = tag_local_name(tag) else {
    return (None, DocxCleanStats::default());
  };
  let mut normalized = None::<String>;
  let mut stats = DocxCleanStats::default();

  match name {
    "style" => normalize_attr(
      tag,
      &mut normalized,
      "type",
      supported_style_type,
      "paragraph",
      &mut stats.style_type_values_normalized,
    ),
    "jc" | "lvlJc" => normalize_attr(
      tag,
      &mut normalized,
      "val",
      supported_justification_value,
      "left",
      &mut stats.justification_values_normalized,
    ),
    "u" => normalize_attr(
      tag,
      &mut normalized,
      "val",
      supported_underline_value,
      "single",
      &mut stats.underline_values_normalized,
    ),
    "highlight" => normalize_attr(
      tag,
      &mut normalized,
      "val",
      supported_highlight_value,
      "yellow",
      &mut stats.highlight_values_normalized,
    ),
    "tab" => {
      normalize_attr(
        tag,
        &mut normalized,
        "val",
        supported_tab_alignment_value,
        "left",
        &mut stats.tab_values_normalized,
      );
      normalize_attr(
        tag,
        &mut normalized,
        "leader",
        supported_tab_leader_value,
        "none",
        &mut stats.tab_values_normalized,
      );
    },
    "type" => normalize_attr(
      tag,
      &mut normalized,
      "val",
      supported_section_type_value,
      "continuous",
      &mut stats.section_values_normalized,
    ),
    "pgSz" => normalize_attr(
      tag,
      &mut normalized,
      "orient",
      supported_page_orientation_value,
      "portrait",
      &mut stats.section_values_normalized,
    ),
    "top" | "left" | "bottom" | "right" | "insideH" | "insideV" | "tl2br" | "tr2bl" | "bar" => {
      normalize_attr(
        tag,
        &mut normalized,
        "val",
        supported_border_value,
        "single",
        &mut stats.border_values_normalized,
      );
    },
    _ => {},
  }

  (normalized, stats)
}

#[hotpath::measure]
fn tag_local_name(tag: &str) -> Option<&str> {
  if tag.starts_with("</") || tag.starts_with("<?") || tag.starts_with("<!") {
    return None;
  }
  let name_end = tag
    .char_indices()
    .find_map(|(ix, ch)| (ch.is_whitespace() || ch == '/' || ch == '>').then_some(ix))
    .unwrap_or(tag.len());
  let name = tag[1..name_end].rsplit(':').next().unwrap_or("");
  (!name.is_empty()).then_some(name)
}

#[hotpath::measure]
fn normalize_attr(
  original_tag: &str,
  normalized_tag: &mut Option<String>,
  attr_name: &str,
  supported: fn(&str) -> bool,
  fallback: &str,
  count: &mut usize,
) {
  let tag = normalized_tag.as_deref().unwrap_or(original_tag);
  let Some((value_start, value_end)) = attr_value_range(tag, attr_name) else {
    return;
  };
  let value = &tag[value_start..value_end];
  if supported(value) {
    return;
  }

  let mut normalized = String::with_capacity(tag.len() + fallback.len().saturating_sub(value.len()));
  normalized.push_str(&tag[..value_start]);
  normalized.push_str(fallback);
  normalized.push_str(&tag[value_end..]);
  *normalized_tag = Some(normalized);
  *count += 1;
}

#[hotpath::measure]
fn attr_value_range(tag: &str, target_attr_name: &str) -> Option<(usize, usize)> {
  let mut cursor = 0_usize;
  while let Some(relative_val) = tag[cursor..].find(target_attr_name) {
    let val_start = cursor + relative_val;
    let attr_name_start = tag[..val_start]
      .char_indices()
      .rev()
      .find_map(|(ix, ch)| (ch.is_whitespace() || ch == '<').then_some(ix + ch.len_utf8()))
      .unwrap_or(0);
    let attr_name = &tag[attr_name_start..val_start + target_attr_name.len()];
    if attr_name.rsplit(':').next() != Some(target_attr_name) {
      cursor = val_start + target_attr_name.len();
      continue;
    }
    let mut ix = val_start + target_attr_name.len();
    while ix < tag.len() && tag[ix..].chars().next().is_some_and(char::is_whitespace) {
      ix += tag[ix..].chars().next().unwrap().len_utf8();
    }
    if !tag[ix..].starts_with('=') {
      cursor = ix;
      continue;
    }
    ix += 1;
    while ix < tag.len() && tag[ix..].chars().next().is_some_and(char::is_whitespace) {
      ix += tag[ix..].chars().next().unwrap().len_utf8();
    }
    let quote = tag[ix..].chars().next()?;
    if quote != '"' && quote != '\'' {
      cursor = ix;
      continue;
    }
    let value_start = ix + quote.len_utf8();
    let value_end = tag[value_start..]
      .find(quote)
      .map(|relative| value_start + relative)?;
    return Some((value_start, value_end));
  }
  None
}

#[hotpath::measure]
fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
  !needle.is_empty()
    && haystack
      .windows(needle.len())
      .any(|window| window == needle)
}

impl DocxCleanStats {
  #[hotpath::measure]
  const fn has_changes(self) -> bool {
    self.underline_values_normalized
      + self.highlight_values_normalized
      + self.border_values_normalized
      + self.justification_values_normalized
      + self.tab_values_normalized
      + self.section_values_normalized
      + self.style_type_values_normalized
      > 0
  }

  #[hotpath::measure]
  const fn merge(&mut self, other: Self) {
    self.underline_values_normalized += other.underline_values_normalized;
    self.highlight_values_normalized += other.highlight_values_normalized;
    self.border_values_normalized += other.border_values_normalized;
    self.justification_values_normalized += other.justification_values_normalized;
    self.tab_values_normalized += other.tab_values_normalized;
    self.section_values_normalized += other.section_values_normalized;
    self.style_type_values_normalized += other.style_type_values_normalized;
  }
}

#[hotpath::measure]
fn supported_style_type(value: &str) -> bool {
  matches!(value, "paragraph" | "character" | "table" | "numbering")
}

#[hotpath::measure]
fn supported_justification_value(value: &str) -> bool {
  matches!(value, "start" | "left" | "end" | "right" | "center" | "both" | "justify" | "distribute")
}

#[hotpath::measure]
fn supported_underline_value(value: &str) -> bool {
  matches!(
    value,
    "none" | "single" | "words" | "double" | "thick" | "dotted" | "dash" | "dotDash" | "dotDotDash" | "wave"
  )
}

#[hotpath::measure]
fn supported_highlight_value(value: &str) -> bool {
  matches!(
    value,
    "black"
      | "blue"
      | "cyan"
      | "darkBlue"
      | "darkCyan"
      | "darkGray"
      | "darkGreen"
      | "darkMagenta"
      | "darkRed"
      | "darkYellow"
      | "green"
      | "lightGray"
      | "magenta"
      | "none"
      | "red"
      | "white"
      | "yellow"
  )
}

#[hotpath::measure]
fn supported_border_value(value: &str) -> bool {
  matches!(
    value,
    "none"
      | "nil"
      | "single"
      | "thick"
      | "double"
      | "dotted"
      | "dashed"
      | "dotDash"
      | "dotDotDash"
      | "triple"
      | "thinThickSmallGap"
      | "thickThinSmallGap"
      | "thinThickMediumGap"
      | "thickThinMediumGap"
      | "thinThickLargeGap"
      | "thickThinLargeGap"
      | "wave"
      | "doubleWave"
      | "threeDEmboss"
      | "threeDEngrave"
      | "outset"
      | "inset"
  )
}

#[hotpath::measure]
fn supported_tab_alignment_value(value: &str) -> bool {
  matches!(value, "left" | "start" | "center" | "right" | "end" | "decimal" | "bar" | "clear" | "num")
}

#[hotpath::measure]
fn supported_tab_leader_value(value: &str) -> bool {
  matches!(value, "none" | "dot" | "hyphen" | "underscore" | "heavy" | "middleDot")
}

#[hotpath::measure]
fn supported_section_type_value(value: &str) -> bool {
  matches!(value, "nextPage" | "continuous" | "evenPage" | "oddPage" | "nextColumn")
}

#[hotpath::measure]
fn supported_page_orientation_value(value: &str) -> bool {
  matches!(value, "portrait" | "landscape")
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  #[hotpath::measure]
  fn unsupported_underline_values_normalize_to_single() {
    let xml = r#"<w:rPr><w:u w:val="dashHeavy"/><w:u w:val='wavyDouble'/><w:u w:val="none"/></w:rPr>"#;
    let (normalized, stats) = normalize_formatting_values_in_xml(xml);
    let normalized = normalized.as_deref().unwrap_or(xml);

    assert_eq!(stats.underline_values_normalized, 2);
    assert!(normalized.contains(r#"<w:u w:val="single"/>"#));
    assert!(normalized.contains(r"<w:u w:val='single'/>"));
    assert!(normalized.contains(r#"<w:u w:val="none"/>"#));
  }

  #[test]
  #[hotpath::measure]
  fn supported_underline_values_are_preserved() {
    let xml = r#"<w:rPr><w:u w:val="dash"/><w:u w:val="wave"/></w:rPr>"#;
    let (normalized, stats) = normalize_formatting_values_in_xml(xml);

    assert_eq!(stats.underline_values_normalized, 0);
    assert!(normalized.is_none());
  }

  #[test]
  #[hotpath::measure]
  fn unsupported_parser_enum_values_are_normalized() {
    let xml = r#"<w:style w:type="weird"><w:jc w:val="thaiDistribute"/><w:highlight w:val="pink"/><w:top w:val="dashSmallGap"/><w:tab w:val="list" w:leader="equals"/><w:type w:val="other"/><w:pgSz w:orient="sideways"/></w:style>"#;
    let (normalized, stats) = normalize_formatting_values_in_xml(xml);
    let normalized = normalized.as_deref().unwrap_or(xml);

    assert_eq!(stats.style_type_values_normalized, 1);
    assert_eq!(stats.justification_values_normalized, 1);
    assert_eq!(stats.highlight_values_normalized, 1);
    assert_eq!(stats.border_values_normalized, 1);
    assert_eq!(stats.tab_values_normalized, 2);
    assert_eq!(stats.section_values_normalized, 2);
    assert!(normalized.contains(r#"w:type="paragraph""#));
    assert!(normalized.contains(r#"<w:jc w:val="left"/>"#));
    assert!(normalized.contains(r#"<w:highlight w:val="yellow"/>"#));
    assert!(normalized.contains(r#"<w:top w:val="single"/>"#));
    assert!(normalized.contains(r#"<w:tab w:val="left" w:leader="none"/>"#));
    assert!(normalized.contains(r#"<w:type w:val="continuous"/>"#));
    assert!(normalized.contains(r#"<w:pgSz w:orient="portrait"/>"#));
  }
}
