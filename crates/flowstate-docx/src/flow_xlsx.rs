//! H1: a minimal .xlsx writer for FLOW export — one worksheet per flow sheet,
//! row 1 = the speech headers, inline strings throughout (no sharedStrings
//! table, no styles beyond the required stub). The shape deliberately mirrors
//! a hand-made Excel flow so coaches on Verbatim-era workflows open it
//! natively; the exact Verbatim template refinement lands with the H2 import
//! work once the Verbatim source is in hand.
//!
//! Input is format-agnostic (names + string grids) so this crate needs no
//! dependency on flowstate-flow.

use std::fmt::Write as _;
use std::io::{self, Cursor, Write};

use zip::{CompressionMethod, ZipWriter, write::FileOptions};

/// One worksheet: a name, the header row, and the body grid (None = empty).
pub struct XlsxSheet {
  pub name: String,
  pub headers: Vec<String>,
  pub rows: Vec<Vec<Option<String>>>,
}

/// Serialize the sheets as a complete .xlsx package.
pub fn write_xlsx(sheets: &[XlsxSheet]) -> io::Result<Vec<u8>> {
  let mut buffer = Cursor::new(Vec::new());
  let mut zip = ZipWriter::new(&mut buffer);
  let options = FileOptions::default().compression_method(CompressionMethod::Deflated);
  let write_part = |zip: &mut ZipWriter<&mut Cursor<Vec<u8>>>, name: &str, body: String| -> io::Result<()> {
    zip.start_file(name, options).map_err(io::Error::other)?;
    zip.write_all(body.as_bytes())
  };

  // [Content_Types].xml
  let mut content_types = String::from(
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
"#,
  );
  for index in 1..=sheets.len().max(1) {
    let _ = writeln!(
      content_types,
      "<Override PartName=\"/xl/worksheets/sheet{index}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/>"
    );
  }
  content_types.push_str("</Types>");
  write_part(&mut zip, "[Content_Types].xml", content_types)?;

  // _rels/.rels
  write_part(
    &mut zip,
    "_rels/.rels",
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"#
      .to_string(),
  )?;

  // xl/workbook.xml + its rels
  let mut workbook = String::from(
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets>
"#,
  );
  let mut workbook_rels = String::from(
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
"#,
  );
  let effective: Vec<&XlsxSheet> = if sheets.is_empty() { Vec::new() } else { sheets.iter().collect() };
  for (index, sheet) in effective.iter().enumerate() {
    let number = index + 1;
    let _ = writeln!(
      workbook,
      "<sheet name=\"{}\" sheetId=\"{number}\" r:id=\"rId{number}\"/>",
      escape_xml(&sanitize_sheet_name(&sheet.name, index))
    );
    let _ = writeln!(
      workbook_rels,
      "<Relationship Id=\"rId{number}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet{number}.xml\"/>"
    );
  }
  if effective.is_empty() {
    workbook.push_str("<sheet name=\"Flow\" sheetId=\"1\" r:id=\"rId1\"/>\n");
    workbook_rels.push_str(
      "<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet1.xml\"/>\n",
    );
  }
  workbook.push_str("</sheets></workbook>");
  workbook_rels.push_str("</Relationships>");
  write_part(&mut zip, "xl/workbook.xml", workbook)?;
  write_part(&mut zip, "xl/_rels/workbook.xml.rels", workbook_rels)?;

  // Worksheets: inline strings, sequential cells (the optional `r` refs are
  // omitted — consumers fill row-major, which is exactly our grid).
  if effective.is_empty() {
    write_part(&mut zip, "xl/worksheets/sheet1.xml", empty_worksheet())?;
  }
  for (index, sheet) in effective.iter().enumerate() {
    let mut body = String::from(
      r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>
"#,
    );
    body.push_str("<row>");
    for header in &sheet.headers {
      push_inline_cell(&mut body, header);
    }
    body.push_str("</row>\n");
    for row in &sheet.rows {
      body.push_str("<row>");
      for cell in row {
        match cell {
          Some(text) => push_inline_cell(&mut body, text),
          None => body.push_str("<c/>"),
        }
      }
      body.push_str("</row>\n");
    }
    body.push_str("</sheetData></worksheet>");
    write_part(&mut zip, &format!("xl/worksheets/sheet{}.xml", index + 1), body)?;
  }

  zip.finish().map_err(io::Error::other)?;
  // `finish` borrows &mut self in this zip version — release the writer (and
  // its borrow of `buffer`) explicitly.
  drop(zip);
  Ok(buffer.into_inner())
}

fn empty_worksheet() -> String {
  r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData/></worksheet>"#
    .to_string()
}

fn push_inline_cell(body: &mut String, text: &str) {
  body.push_str("<c t=\"inlineStr\"><is><t xml:space=\"preserve\">");
  body.push_str(&escape_xml(text));
  body.push_str("</t></is></c>");
}

/// Excel sheet-name law: ≤31 chars, no []:*?/\ and never empty.
fn sanitize_sheet_name(name: &str, index: usize) -> String {
  let cleaned: String = name
    .chars()
    .filter(|c| !matches!(c, '[' | ']' | ':' | '*' | '?' | '/' | '\\'))
    .take(31)
    .collect();
  let cleaned = cleaned.trim().to_string();
  if cleaned.is_empty() { format!("Sheet{}", index + 1) } else { cleaned }
}

fn escape_xml(text: &str) -> String {
  let mut escaped = String::with_capacity(text.len());
  for c in text.chars() {
    match c {
      '&' => escaped.push_str("&amp;"),
      '<' => escaped.push_str("&lt;"),
      '>' => escaped.push_str("&gt;"),
      '"' => escaped.push_str("&quot;"),
      '\'' => escaped.push_str("&apos;"),
      _ => escaped.push(c),
    }
  }
  escaped
}

// ---- H2: import (generic — the Verbatim-template shaping refines this once
// the Verbatim source is in hand) --------------------------------------------

use std::io::Read as _;

/// Read an .xlsx into sheets of strings. Handles inline strings, shared
/// strings, and plain `<v>` values; honors explicit `r` cell references so
/// sparse rows land in the right columns. Defensive: unknown parts and
/// malformed cells degrade to empty, never to an error, wherever safe.
pub fn read_xlsx(bytes: &[u8]) -> io::Result<Vec<XlsxSheet>> {
  let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|error| io::Error::other(format!("not an xlsx (zip): {error}")))?;
  let read_part = |archive: &mut zip::ZipArchive<Cursor<&[u8]>>, name: &str| -> Option<String> {
    let mut file = archive.by_name(name).ok()?;
    let mut body = String::new();
    file.read_to_string(&mut body).ok()?;
    Some(body)
  };
  let workbook = read_part(&mut archive, "xl/workbook.xml")
    .ok_or_else(|| io::Error::other("xlsx has no xl/workbook.xml"))?;
  let rels = read_part(&mut archive, "xl/_rels/workbook.xml.rels").unwrap_or_default();
  let shared = read_part(&mut archive, "xl/sharedStrings.xml")
    .map(|xml| parse_shared_strings(&xml))
    .unwrap_or_default();
  let rel_targets = parse_workbook_rels(&rels);
  let mut sheets = Vec::new();
  for (name, rid) in parse_workbook_sheets(&workbook) {
    let target = rel_targets
      .get(&rid)
      .cloned()
      .unwrap_or_else(|| format!("worksheets/sheet{}.xml", sheets.len() + 1));
    let part = if target.starts_with('/') {
      target.trim_start_matches('/').to_string()
    } else {
      format!("xl/{target}")
    };
    let Some(xml) = read_part(&mut archive, &part) else { continue };
    let grid = parse_worksheet(&xml, &shared);
    let mut rows = grid.into_iter();
    let headers: Vec<String> = rows
      .next()
      .map(|row| row.into_iter().map(|cell| cell.unwrap_or_default()).collect())
      .unwrap_or_default();
    sheets.push(XlsxSheet {
      name,
      headers,
      rows: rows.collect(),
    });
  }
  if sheets.is_empty() {
    return Err(io::Error::other("xlsx contains no readable worksheets"));
  }
  Ok(sheets)
}

/// Read delimited text (CSV/TSV) as ONE sheet: tab-delimited when any tab
/// exists, else comma with minimal quote handling.
pub fn read_delimited(name: &str, text: &str) -> XlsxSheet {
  let delimiter = if text.contains('\t') { '\t' } else { ',' };
  let mut lines = text.replace("\r\n", "\n");
  if lines.ends_with('\n') {
    lines.pop();
  }
  let mut rows: Vec<Vec<Option<String>>> = lines
    .split('\n')
    .map(|line| {
      split_delimited(line, delimiter)
        .into_iter()
        .map(|field| if field.is_empty() { None } else { Some(field) })
        .collect()
    })
    .collect();
  let headers = if rows.is_empty() {
    Vec::new()
  } else {
    rows.remove(0).into_iter().map(|cell| cell.unwrap_or_default()).collect()
  };
  XlsxSheet {
    name: name.to_string(),
    headers,
    rows,
  }
}

/// Minimal CSV field splitting: quotes wrap fields, doubled quotes escape.
fn split_delimited(line: &str, delimiter: char) -> Vec<String> {
  let mut fields = Vec::new();
  let mut current = String::new();
  let mut in_quotes = false;
  let mut chars = line.chars().peekable();
  while let Some(c) = chars.next() {
    if in_quotes {
      if c == '"' {
        if chars.peek() == Some(&'"') {
          chars.next();
          current.push('"');
        } else {
          in_quotes = false;
        }
      } else {
        current.push(c);
      }
    } else if c == '"' && current.is_empty() {
      in_quotes = true;
    } else if c == delimiter {
      fields.push(std::mem::take(&mut current));
    } else {
      current.push(c);
    }
  }
  fields.push(current);
  fields
}

/// `<sheet name=".." r:id="rIdN"/>` pairs, in workbook order.
fn parse_workbook_sheets(xml: &str) -> Vec<(String, String)> {
  let mut reader = quick_xml::Reader::from_str(xml);
  let mut out = Vec::new();
  loop {
    match reader.read_event() {
      Ok(quick_xml::events::Event::Start(e)) | Ok(quick_xml::events::Event::Empty(e)) if e.local_name().as_ref() == b"sheet" => {
        let mut name = String::new();
        let mut rid = String::new();
        for attr in e.attributes().flatten() {
          let key = attr.key.local_name();
          if key.as_ref() == b"name" {
            name = attr.normalized_value(quick_xml::XmlVersion::Implicit1_0).unwrap_or_default().to_string();
          } else if key.as_ref() == b"id" {
            rid = attr.normalized_value(quick_xml::XmlVersion::Implicit1_0).unwrap_or_default().to_string();
          }
        }
        out.push((if name.is_empty() { format!("Sheet{}", out.len() + 1) } else { name }, rid));
      },
      Ok(quick_xml::events::Event::Eof) | Err(_) => break,
      _ => {},
    }
  }
  out
}

/// `rId -> Target` from workbook.xml.rels.
fn parse_workbook_rels(xml: &str) -> std::collections::HashMap<String, String> {
  let mut reader = quick_xml::Reader::from_str(xml);
  let mut out = std::collections::HashMap::new();
  loop {
    match reader.read_event() {
      Ok(quick_xml::events::Event::Start(e)) | Ok(quick_xml::events::Event::Empty(e))
        if e.local_name().as_ref() == b"Relationship" =>
      {
        let mut id = String::new();
        let mut target = String::new();
        for attr in e.attributes().flatten() {
          let key = attr.key.local_name();
          if key.as_ref() == b"Id" {
            id = attr.normalized_value(quick_xml::XmlVersion::Implicit1_0).unwrap_or_default().to_string();
          } else if key.as_ref() == b"Target" {
            target = attr.normalized_value(quick_xml::XmlVersion::Implicit1_0).unwrap_or_default().to_string();
          }
        }
        if !id.is_empty() {
          out.insert(id, target);
        }
      },
      Ok(quick_xml::events::Event::Eof) | Err(_) => break,
      _ => {},
    }
  }
  out
}

/// sharedStrings: each `<si>` becomes the concatenation of its `<t>` runs.
fn parse_shared_strings(xml: &str) -> Vec<String> {
  let mut reader = quick_xml::Reader::from_str(xml);
  let mut out = Vec::new();
  let mut current: Option<String> = None;
  let mut in_text = false;
  loop {
    match reader.read_event() {
      Ok(quick_xml::events::Event::Start(e)) => match e.local_name().as_ref() {
        b"si" => current = Some(String::new()),
        b"t" => in_text = true,
        _ => {},
      },
      Ok(quick_xml::events::Event::Text(text)) => {
        if in_text && let Some(current) = current.as_mut() {
          current.push_str(&text.xml10_content().unwrap_or_default());
        }
      },
      Ok(quick_xml::events::Event::GeneralRef(reference)) => {
        if in_text && let Some(current) = current.as_mut() {
          current.push_str(&resolve_entity(&reference));
        }
      },
      Ok(quick_xml::events::Event::End(e)) => match e.local_name().as_ref() {
        b"si" => {
          if let Some(done) = current.take() {
            out.push(done);
          }
        },
        b"t" => in_text = false,
        _ => {},
      },
      Ok(quick_xml::events::Event::Eof) | Err(_) => break,
      _ => {},
    }
  }
  out
}

/// One worksheet's cells as a row-major grid of Option<String>.
fn parse_worksheet(xml: &str, shared: &[String]) -> Vec<Vec<Option<String>>> {
  let mut reader = quick_xml::Reader::from_str(xml);
  let mut grid: Vec<Vec<Option<String>>> = Vec::new();
  let mut row_ix = 0usize; // 1-based rows in refs; this is the NEXT sequential row
  let mut col_ix = 0usize;
  let mut cell_type = String::new();
  let mut in_value = false;
  let mut in_inline_text = false;
  let mut pending: Option<(usize, usize, String)> = None;
  let place = |grid: &mut Vec<Vec<Option<String>>>, row: usize, col: usize, text: String| {
    if text.is_empty() {
      return;
    }
    while grid.len() <= row {
      grid.push(Vec::new());
    }
    let row_cells = &mut grid[row];
    while row_cells.len() <= col {
      row_cells.push(None);
    }
    row_cells[col] = Some(text);
  };
  loop {
    match reader.read_event() {
      Ok(quick_xml::events::Event::Start(e)) => match e.local_name().as_ref() {
        b"row" => {
          let mut explicit = None;
          for attr in e.attributes().flatten() {
            if attr.key.local_name().as_ref() == b"r"
              && let Ok(value) = attr.normalized_value(quick_xml::XmlVersion::Implicit1_0).unwrap_or_default().parse::<usize>()
            {
              explicit = Some(value.saturating_sub(1));
            }
          }
          row_ix = explicit.unwrap_or(row_ix);
          col_ix = 0;
        },
        b"c" => {
          cell_type.clear();
          let mut explicit_col = None;
          for attr in e.attributes().flatten() {
            let key = attr.key.local_name();
            if key.as_ref() == b"t" {
              cell_type = attr.normalized_value(quick_xml::XmlVersion::Implicit1_0).unwrap_or_default().to_string();
            } else if key.as_ref() == b"r" {
              explicit_col = column_from_reference(&attr.normalized_value(quick_xml::XmlVersion::Implicit1_0).unwrap_or_default());
            }
          }
          col_ix = explicit_col.unwrap_or(col_ix);
        },
        b"v" => in_value = true,
        b"t" => in_inline_text = true,
        _ => {},
      },
      // Self-closing tags fire ONLY this event — an empty `<c/>` still
      // advances the column, or the whole row shifts left.
      Ok(quick_xml::events::Event::Empty(e)) => {
        if e.local_name().as_ref() == b"c" {
          let mut explicit_col = None;
          for attr in e.attributes().flatten() {
            if attr.key.local_name().as_ref() == b"r" {
              explicit_col = column_from_reference(&attr.normalized_value(quick_xml::XmlVersion::Implicit1_0).unwrap_or_default());
            }
          }
          col_ix = explicit_col.unwrap_or(col_ix) + 1;
        }
      },
      Ok(quick_xml::events::Event::Text(text)) => {
        if in_value || in_inline_text {
          let raw = text.xml10_content().unwrap_or_default().to_string();
          let resolved = if in_value && cell_type == "s" {
            raw
              .trim()
              .parse::<usize>()
              .ok()
              .and_then(|ix| shared.get(ix).cloned())
              .unwrap_or_default()
          } else {
            raw
          };
          match pending.as_mut() {
            Some((_, _, existing)) => existing.push_str(&resolved),
            None => pending = Some((row_ix, col_ix, resolved)),
          }
        }
      },
      Ok(quick_xml::events::Event::GeneralRef(reference)) => {
        if in_value || in_inline_text {
          let resolved = resolve_entity(&reference);
          match pending.as_mut() {
            Some((_, _, existing)) => existing.push_str(&resolved),
            None => pending = Some((row_ix, col_ix, resolved)),
          }
        }
      },
      Ok(quick_xml::events::Event::End(e)) => match e.local_name().as_ref() {
        b"v" => in_value = false,
        b"t" => in_inline_text = false,
        b"c" => {
          if let Some((row, col, text)) = pending.take() {
            place(&mut grid, row, col, text);
          }
          col_ix += 1;
        },
        b"row" => {
          row_ix += 1;
        },
        _ => {},
      },
      Ok(quick_xml::events::Event::Eof) | Err(_) => break,
      _ => {},
    }
  }
  grid
}

/// quick-xml 0.41 emits `&amp;`-style references as SEPARATE `GeneralRef`
/// events — dropping them silently loses `&`, `<`, quotes from cell text.
fn resolve_entity(reference: &quick_xml::events::BytesRef<'_>) -> String {
  if let Ok(Some(c)) = reference.resolve_char_ref() {
    return c.to_string();
  }
  match &reference[..] {
    b"lt" => "<".to_string(),
    b"gt" => ">".to_string(),
    b"amp" => "&".to_string(),
    b"quot" => "\"".to_string(),
    b"apos" => "'".to_string(),
    _ => String::new(),
  }
}

/// "C7" → column 2. Letters only prefix, 1-based → 0-based.
fn column_from_reference(reference: &str) -> Option<usize> {
  let letters: String = reference.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
  if letters.is_empty() {
    return None;
  }
  let mut value = 0usize;
  for c in letters.chars() {
    value = value * 26 + (c.to_ascii_uppercase() as usize - 'A' as usize + 1);
  }
  Some(value - 1)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn xlsx_package_has_the_required_parts() {
    let bytes = write_xlsx(&[XlsxSheet {
      name: "Case".into(),
      headers: vec!["1AC".into(), "1NC".into()],
      rows: vec![vec![Some("warming adv".into()), None], vec![None, Some("T: reduce".into())]],
    }])
    .expect("xlsx writes");
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("valid zip");
    for part in [
      "[Content_Types].xml",
      "_rels/.rels",
      "xl/workbook.xml",
      "xl/_rels/workbook.xml.rels",
      "xl/worksheets/sheet1.xml",
    ] {
      archive.by_name(part).unwrap_or_else(|_| panic!("missing part {part}"));
    }
  }

  #[test]
  fn sheet_names_sanitize() {
    assert_eq!(sanitize_sheet_name("A[very]:bad*name?", 0), "Averybadname");
    assert_eq!(sanitize_sheet_name("", 2), "Sheet3");
  }

  /// H2: the writer's output reads back losslessly — the import net.
  #[test]
  fn xlsx_round_trips_through_the_reader() {
    let bytes = write_xlsx(&[XlsxSheet {
      name: "Case".into(),
      headers: vec!["1AC".into(), "1NC".into(), "2AC".into()],
      rows: vec![
        vec![Some("warming adv".into()), None, Some("extend Mora".into())],
        vec![None, Some("T: reduce & \"quotes\"".into()), None],
      ],
    }])
    .expect("writes");
    let sheets = read_xlsx(&bytes).expect("reads back");
    assert_eq!(sheets.len(), 1);
    assert_eq!(sheets[0].name, "Case");
    assert_eq!(sheets[0].headers, vec!["1AC", "1NC", "2AC"]);
    assert_eq!(sheets[0].rows.len(), 2);
    assert_eq!(sheets[0].rows[0][0].as_deref(), Some("warming adv"));
    assert_eq!(sheets[0].rows[0][2].as_deref(), Some("extend Mora"));
    assert_eq!(sheets[0].rows[1][1].as_deref(), Some("T: reduce & \"quotes\""));
  }

  #[test]
  fn delimited_import_splits_and_unquotes() {
    let sheet = read_delimited("paste", "1AC,1NC\n\"a, card\",answer\n,late");
    assert_eq!(sheet.headers, vec!["1AC", "1NC"]);
    assert_eq!(sheet.rows[0][0].as_deref(), Some("a, card"));
    assert_eq!(sheet.rows[1][0], None);
    assert_eq!(sheet.rows[1][1].as_deref(), Some("late"));
  }

  #[test]
  fn column_references_decode() {
    assert_eq!(column_from_reference("A1"), Some(0));
    assert_eq!(column_from_reference("C7"), Some(2));
    assert_eq!(column_from_reference("AA3"), Some(26));
    assert_eq!(column_from_reference("7"), None);
  }
}
