//! Structured DOCX body import (§26): walks `w:body` in document order and emits
//! the full block model — paragraphs, tables, images and equations — rather than
//! the flat paragraph stream produced by [`super::interpret_cleaned_docx`].
//!
//! Paragraph recognition is *not* duplicated here. `docx.paragraphs()` (and the
//! pre-computed [`DocumentParagraphInput`] slice it backs) enumerates exactly the
//! top-level `w:p` children of `w:body` in order — table-cell paragraphs are
//! filtered out by `rdocx` (`CT_Body::paragraphs` only keeps `BodyContent::Paragraph`).
//! The walk therefore consumes one slice entry per top-level `w:p` it encounters,
//! keeping the rich paragraph/run heuristics intact, and recognizes table-cell
//! paragraphs through a deliberately small independent path (plain runs + Normal
//! style).
//!
//! Ordering choice: a top-level paragraph that also carries inline drawings or
//! Office Math is emitted as the paragraph block first, then one image block per
//! drawing, then one equation block per `m:oMath`. Empty paragraphs that exist
//! only to host an object are still emitted (kept simple to preserve cursor
//! alignment) and render as a blank line next to the object.

use std::{io, io::Cursor, sync::Arc};

use quick_xml::{
  Reader as XmlReader,
  events::{BytesStart, Event},
};
use rdocx_opc::OpcPackage;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::cleaner::CleanedDocx;
use flowstate_document::{
  AssetId, AssetRecord, DocumentParagraphInput, InputBlock, InputBlockAlignment, InputImageBlock, InputImageSizing, InputParagraph, InputRun,
  InputTableBlock, InputTableCell, InputTableCellBlock, InputTableColumnWidth, InputTableRow, InputTableStyle, ParagraphStyle, RunStyles,
};

use super::omml;

/// Structured import result. Blocks are in body order; `assets` are the deduped
/// image assets keyed by a content-stable [`AssetId`] for insertion into the
/// projection's asset store.
#[derive(Default)]
pub(super) struct StructuredDocx {
  pub(super) blocks: Vec<InputBlock>,
  pub(super) assets: Vec<(AssetId, AssetRecord)>,
  pub(super) tables_imported: usize,
  pub(super) images_imported: usize,
  pub(super) equations_imported: usize,
}

#[hotpath::measure]
pub(super) fn interpret_structured(cleaned: &CleanedDocx, paragraphs: &[DocumentParagraphInput]) -> io::Result<StructuredDocx> {
  // The package carries the main-document relationships and media parts needed
  // to resolve embedded images; a missing/unreadable package simply yields no
  // image assets rather than failing the import.
  let package = OpcPackage::from_reader(Cursor::new(cleaned.bytes.as_slice())).ok();
  let main_part = package.as_ref().and_then(OpcPackage::main_document_part);

  let owned_xml;
  let doc_xml: &[u8] = match cleaned.main_document_xml.as_deref() {
    Some(xml) => xml,
    None => {
      let Some((package, main_part)) = package.as_ref().zip(main_part.as_deref()) else {
        return Ok(StructuredDocx::default());
      };
      let Some(part) = package.get_part(main_part) else {
        return Ok(StructuredDocx::default());
      };
      owned_xml = part.to_vec();
      &owned_xml
    },
  };

  let Some(root) = parse_tree(doc_xml) else {
    return Ok(StructuredDocx::default());
  };
  let Some(body) = child(&root, "body") else {
    return Ok(StructuredDocx::default());
  };

  let mut walker = StructuredWalker {
    doc_xml,
    paragraphs,
    package: package.as_ref(),
    main_part: main_part.as_deref(),
    cursor: 0,
    blocks: Vec::new(),
    assets: Vec::new(),
    emitted_assets: FxHashSet::default(),
    tables_imported: 0,
    images_imported: 0,
    equations_imported: 0,
  };
  walker.walk_body(body);

  Ok(StructuredDocx {
    blocks: walker.blocks,
    assets: walker.assets,
    tables_imported: walker.tables_imported,
    images_imported: walker.images_imported,
    equations_imported: walker.equations_imported,
  })
}

struct StructuredWalker<'ctx> {
  doc_xml: &'ctx [u8],
  paragraphs: &'ctx [DocumentParagraphInput],
  package: Option<&'ctx OpcPackage>,
  main_part: Option<&'ctx str>,
  cursor: usize,
  blocks: Vec<InputBlock>,
  assets: Vec<(AssetId, AssetRecord)>,
  emitted_assets: FxHashSet<AssetId>,
  tables_imported: usize,
  images_imported: usize,
  equations_imported: usize,
}

impl StructuredWalker<'_> {
  fn walk_body(&mut self, body: &XmlNode) {
    for node in &body.children {
      match node.local.as_str() {
        "p" => self.walk_paragraph(node),
        "tbl" => {
          let table = self.parse_table(node);
          self.blocks.push(InputBlock::Table(table));
        },
        _ => {},
      }
    }
  }

  fn walk_paragraph(&mut self, paragraph_node: &XmlNode) {
    // Top-level paragraphs are 1:1 with the pre-computed slice; the cursor only
    // advances on `w:p`, so tables, section properties and SDTs never desync it.
    let paragraph = if self.cursor < self.paragraphs.len() {
      let input = input_paragraph_from_document(&self.paragraphs[self.cursor]);
      self.cursor += 1;
      input
    } else {
      cell_paragraph(paragraph_node)
    };
    self.blocks.push(InputBlock::Paragraph(paragraph));

    let mut drawings = Vec::new();
    collect_descendants(paragraph_node, "drawing", &mut drawings);
    for drawing in drawings {
      if let Some(image) = self.image_from_drawing(drawing) {
        self.blocks.push(InputBlock::Image(image));
        self.images_imported += 1;
      }
    }

    if let Some(bytes) = self.doc_xml.get(paragraph_node.start..paragraph_node.end)
      && omml::contains_office_math(bytes)
    {
      for equation in omml::equations_from_container_bytes(bytes) {
        self.blocks.push(InputBlock::Equation(equation));
        self.equations_imported += 1;
      }
    }
  }

  fn parse_table(&mut self, node: &XmlNode) -> InputTableBlock {
    self.tables_imported += 1;
    let column_widths = table_column_widths(node);
    let mut rows: Vec<InputTableRow> = Vec::new();
    let mut header_row = false;
    let mut first_row = true;
    // grid column -> (row index, cell index) of the cell that started a vertical
    // merge, so continuation cells fold into its `row_span` instead of emitting.
    let mut vertical_open: FxHashMap<usize, (usize, usize)> = FxHashMap::default();

    for row_node in &node.children {
      if row_node.local != "tr" {
        continue;
      }
      if first_row {
        first_row = false;
        header_row = row_is_header(row_node);
      }
      let mut cells: Vec<InputTableCell> = Vec::new();
      let mut grid_col = 0_usize;
      for cell_node in &row_node.children {
        if cell_node.local == "tc" {
          grid_col = self.add_table_cell(cell_node, grid_col, &mut rows, &mut cells, &mut vertical_open);
        }
      }
      rows.push(InputTableRow { cells });
    }

    InputTableBlock {
      rows,
      column_widths,
      style: InputTableStyle { header_row },
    }
  }

  /// Appends one `w:tc` to the current row (or folds a vertical-merge
  /// continuation into the originating cell above) and returns the next grid
  /// column. `rows` holds the already-completed rows; `cells` is the row in
  /// progress.
  fn add_table_cell(
    &mut self,
    cell_node: &XmlNode,
    grid_col: usize,
    rows: &mut [InputTableRow],
    cells: &mut Vec<InputTableCell>,
    vertical_open: &mut FxHashMap<usize, (usize, usize)>,
  ) -> usize {
    let col_span = cell_grid_span(cell_node);
    match cell_vertical_merge(cell_node) {
      VerticalMerge::Continue => {
        if let Some(&(row_ix, cell_ix)) = vertical_open.get(&grid_col)
          && let Some(row) = rows.get_mut(row_ix)
          && let Some(cell) = row.cells.get_mut(cell_ix)
        {
          cell.row_span = cell.row_span.saturating_add(1);
        }
      },
      VerticalMerge::Restart => {
        vertical_open.insert(grid_col, (rows.len(), cells.len()));
        let blocks = self.cell_blocks(cell_node);
        cells.push(InputTableCell {
          blocks,
          row_span: 1,
          col_span,
        });
      },
      VerticalMerge::None => {
        vertical_open.remove(&grid_col);
        let blocks = self.cell_blocks(cell_node);
        cells.push(InputTableCell {
          blocks,
          row_span: 1,
          col_span,
        });
      },
    }
    grid_col + usize::from(col_span.max(1))
  }

  fn cell_blocks(&mut self, cell_node: &XmlNode) -> Vec<InputTableCellBlock> {
    let mut blocks = Vec::new();
    for node in &cell_node.children {
      match node.local.as_str() {
        "p" => blocks.push(InputTableCellBlock::Paragraph(cell_paragraph(node))),
        "tbl" => blocks.push(InputTableCellBlock::Table(self.parse_table(node))),
        _ => {},
      }
    }
    if blocks.is_empty() {
      blocks.push(InputTableCellBlock::Paragraph(InputParagraph {
        style: ParagraphStyle::Normal,
        runs: Vec::new(),
      }));
    }
    blocks
  }

  fn image_from_drawing(&mut self, drawing: &XmlNode) -> Option<InputImageBlock> {
    let relationship_id = find_descendant_with_attr(drawing, "blip", "embed")?.attr("embed")?;
    let asset_id = self.resolve_image(relationship_id)?;
    let alt_text = find_descendant(drawing, "docPr")
      .and_then(|doc_pr| doc_pr.attr("descr").or_else(|| doc_pr.attr("name")))
      .unwrap_or_default()
      .to_owned();
    Some(InputImageBlock {
      asset_id,
      alt_text,
      caption: None,
      sizing: drawing_sizing(drawing),
      alignment: InputBlockAlignment::Left,
    })
  }

  fn resolve_image(&mut self, relationship_id: &str) -> Option<AssetId> {
    let package = self.package?;
    let main_part = self.main_part?;
    let relationship = package
      .get_part_rels(main_part)?
      .get_by_id(relationship_id)?;
    let target = OpcPackage::resolve_rel_target(main_part, &relationship.target);
    let bytes = package.get_part(&target)?;
    let asset_id = asset_id_from_bytes(bytes);
    if self.emitted_assets.insert(asset_id) {
      self.assets.push((
        asset_id,
        AssetRecord {
          id: asset_id,
          mime_type: mime_from_path(&target).into(),
          original_name: Some(file_name(&target).into()),
          content_hash: AssetRecord::stable_content_hash(bytes),
          bytes: Arc::new(bytes.to_vec()),
        },
      ));
    }
    Some(asset_id)
  }
}

/// Vertical-merge state for a table cell (`w:tcPr/w:vMerge`).
enum VerticalMerge {
  None,
  Restart,
  Continue,
}

fn input_paragraph_from_document(paragraph: &DocumentParagraphInput) -> InputParagraph {
  InputParagraph {
    style: paragraph.style,
    runs: paragraph
      .runs
      .iter()
      .map(|run| InputRun {
        text: run.text.clone(),
        styles: run.styles,
      })
      .collect(),
  }
}

/// Lightweight cell-paragraph recognition: table-cell paragraphs are not part of
/// the recognized slice, so they are imported as Normal-styled plain text. This
/// trades run-level semantics inside tables for a small, robust path.
fn cell_paragraph(paragraph_node: &XmlNode) -> InputParagraph {
  let mut text = String::new();
  collect_run_text(paragraph_node, &mut text);
  let runs = if text.is_empty() {
    Vec::new()
  } else {
    vec![InputRun {
      text,
      styles: RunStyles::default(),
    }]
  };
  InputParagraph {
    style: ParagraphStyle::Normal,
    runs,
  }
}

fn collect_run_text(node: &XmlNode, out: &mut String) {
  for child_node in &node.children {
    match child_node.local.as_str() {
      "t" => out.push_str(&child_node.text),
      "tab" => out.push('\t'),
      "br" | "cr" => out.push('\n'),
      _ => collect_run_text(child_node, out),
    }
  }
}

fn table_column_widths(node: &XmlNode) -> Vec<InputTableColumnWidth> {
  let Some(grid) = child(node, "tblGrid") else {
    return Vec::new();
  };
  grid
    .children
    .iter()
    .filter(|candidate| candidate.local == "gridCol")
    .map(|column| {
      column
        .attr("w")
        .and_then(|width| width.parse::<i64>().ok())
        .filter(|twips| *twips > 0)
        .and_then(|twips| u32::try_from(twips.saturating_mul(96) / 1440).ok())
        .map_or(InputTableColumnWidth::Auto, InputTableColumnWidth::FixedPx)
    })
    .collect()
}

fn cell_grid_span(cell_node: &XmlNode) -> u16 {
  child(cell_node, "tcPr")
    .and_then(|properties| child(properties, "gridSpan"))
    .and_then(|grid_span| grid_span.attr("val"))
    .and_then(|value| value.parse::<u16>().ok())
    .filter(|span| *span >= 1)
    .unwrap_or(1)
}

fn cell_vertical_merge(cell_node: &XmlNode) -> VerticalMerge {
  let Some(properties) = child(cell_node, "tcPr") else {
    return VerticalMerge::None;
  };
  let Some(vertical_merge) = child(properties, "vMerge") else {
    return VerticalMerge::None;
  };
  // `<w:vMerge/>` and `w:val="continue"` continue the merge above; only an
  // explicit `restart` opens a new vertical span.
  match vertical_merge.attr("val") {
    Some("restart") => VerticalMerge::Restart,
    _ => VerticalMerge::Continue,
  }
}

fn row_is_header(row_node: &XmlNode) -> bool {
  child(row_node, "trPr")
    .and_then(|properties| child(properties, "tblHeader"))
    .is_some_and(|header| !matches!(header.attr("val"), Some("false" | "0" | "off")))
}

fn drawing_sizing(drawing: &XmlNode) -> InputImageSizing {
  let Some(extent) = find_descendant(drawing, "extent") else {
    return InputImageSizing::Intrinsic;
  };
  let Some(width_px) = extent.attr("cx").and_then(emu_to_px) else {
    return InputImageSizing::Intrinsic;
  };
  InputImageSizing::Fixed {
    width_px,
    height_px: extent.attr("cy").and_then(emu_to_px),
  }
}

fn emu_to_px(value: &str) -> Option<u32> {
  let emu = value.parse::<i64>().ok()?;
  if emu <= 0 {
    return None;
  }
  u32::try_from(emu.saturating_mul(96) / 914_400).ok()
}

fn asset_id_from_bytes(bytes: &[u8]) -> AssetId {
  AssetId(u128::from(AssetRecord::stable_content_hash(bytes)))
}

fn mime_from_path(path: &str) -> &'static str {
  let extension = path
    .rsplit('.')
    .next()
    .unwrap_or_default()
    .to_ascii_lowercase();
  match extension.as_str() {
    "png" => "image/png",
    "jpg" | "jpeg" => "image/jpeg",
    "gif" => "image/gif",
    "bmp" => "image/bmp",
    "tif" | "tiff" => "image/tiff",
    "webp" => "image/webp",
    "svg" => "image/svg+xml",
    "emf" => "image/emf",
    "wmf" => "image/wmf",
    _ => "application/octet-stream",
  }
}

fn file_name(path: &str) -> String {
  path.rsplit('/').next().unwrap_or(path).to_owned()
}

// -- Lightweight DOM with byte spans ---------------------------------------

/// A minimal element tree. `start..end` spans the element (from the opening `<`
/// of its start tag to just past its end tag) in the source XML, so equation
/// subtrees can be handed verbatim to [`omml`].
struct XmlNode {
  local: String,
  attrs: Vec<(String, String)>,
  text: String,
  children: Vec<XmlNode>,
  start: usize,
  end: usize,
}

impl XmlNode {
  fn attr(&self, local: &str) -> Option<&str> {
    self
      .attrs
      .iter()
      .find(|entry| entry.0 == local)
      .map(|entry| entry.1.as_str())
  }
}

fn parse_tree(xml: &[u8]) -> Option<XmlNode> {
  let mut reader = XmlReader::from_reader(xml);
  reader.config_mut().trim_text(false);
  let mut buf = Vec::new();
  let mut stack: Vec<XmlNode> = Vec::new();
  let mut root: Option<XmlNode> = None;

  loop {
    // Captured before the read: for a tag event this is the offset of its `<`.
    let start = reader.buffer_position() as usize;
    match reader.read_event_into(&mut buf) {
      Ok(Event::Start(event)) => {
        let mut node = node_from_start(&event);
        node.start = start;
        stack.push(node);
      },
      Ok(Event::Empty(event)) => {
        let mut node = node_from_start(&event);
        node.start = start;
        node.end = reader.buffer_position() as usize;
        push_node(&mut stack, &mut root, node);
      },
      Ok(Event::Text(event)) => {
        if let Some(top) = stack.last_mut()
          && let Ok(text) = event.unescape()
        {
          top.text.push_str(&text);
        }
      },
      Ok(Event::End(_)) => {
        if let Some(mut node) = stack.pop() {
          node.end = reader.buffer_position() as usize;
          push_node(&mut stack, &mut root, node);
        }
      },
      Ok(Event::Eof) => break,
      Err(_) => return None,
      _ => {},
    }
    buf.clear();
  }

  root
}

fn push_node(stack: &mut [XmlNode], root: &mut Option<XmlNode>, node: XmlNode) {
  if let Some(parent) = stack.last_mut() {
    parent.children.push(node);
  } else {
    root.get_or_insert(node);
  }
}

fn node_from_start(event: &BytesStart<'_>) -> XmlNode {
  let local = local_name(event.name().as_ref()).to_owned();
  let mut attrs = Vec::new();
  for attribute in event.attributes().flatten() {
    let key = local_name(attribute.key.as_ref()).to_owned();
    let value = String::from_utf8_lossy(attribute.value.as_ref()).into_owned();
    attrs.push((key, value));
  }
  XmlNode {
    local,
    attrs,
    text: String::new(),
    children: Vec::new(),
    start: 0,
    end: 0,
  }
}

fn local_name(name: &[u8]) -> &str {
  let name = std::str::from_utf8(name).unwrap_or_default();
  name.rsplit(':').next().unwrap_or(name)
}

fn child<'tree>(node: &'tree XmlNode, local: &str) -> Option<&'tree XmlNode> {
  node
    .children
    .iter()
    .find(|candidate| candidate.local == local)
}

fn find_descendant<'tree>(node: &'tree XmlNode, local: &str) -> Option<&'tree XmlNode> {
  for candidate in &node.children {
    if candidate.local == local {
      return Some(candidate);
    }
    if let Some(found) = find_descendant(candidate, local) {
      return Some(found);
    }
  }
  None
}

fn find_descendant_with_attr<'tree>(node: &'tree XmlNode, local: &str, attr: &str) -> Option<&'tree XmlNode> {
  for candidate in &node.children {
    if candidate.local == local && candidate.attr(attr).is_some() {
      return Some(candidate);
    }
    if let Some(found) = find_descendant_with_attr(candidate, local, attr) {
      return Some(found);
    }
  }
  None
}

fn collect_descendants<'tree>(node: &'tree XmlNode, local: &str, out: &mut Vec<&'tree XmlNode>) {
  for candidate in &node.children {
    if candidate.local == local {
      out.push(candidate);
    } else {
      collect_descendants(candidate, local, out);
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn doc_xml(body: &str) -> Vec<u8> {
    format!(
      r#"<?xml version="1.0"?><w:document xmlns:w="w" xmlns:m="m" xmlns:a="a" xmlns:r="r" xmlns:wp="wp"><w:body>{body}</w:body></w:document>"#
    )
    .into_bytes()
  }

  fn cleaned(body: &str) -> CleanedDocx {
    let xml = doc_xml(body);
    CleanedDocx {
      bytes: Vec::new(),
      main_document_xml: Some(xml),
      report: crate::cleaner::DocxCleanReport {
        stats: crate::cleaner::DocxCleanStats::default(),
        actions: crate::cleaner::CLEANING_RULES,
      },
    }
  }

  fn normal(text: &str) -> DocumentParagraphInput {
    DocumentParagraphInput {
      style: ParagraphStyle::Normal,
      runs: vec![flowstate_document::DocumentRunInput {
        text: text.to_owned(),
        styles: RunStyles::default(),
      }],
    }
  }

  #[test]
  fn table_with_paragraph_cell_is_emitted_in_body_order() {
    let body = r#"<w:p><w:r><w:t>intro</w:t></w:r></w:p><w:tbl><w:tblGrid><w:gridCol w:w="1440"/><w:gridCol w:w="2880"/></w:tblGrid><w:tr><w:trPr><w:tblHeader/></w:trPr><w:tc><w:p><w:r><w:t>left</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>right</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#;
    let cleaned = cleaned(body);
    let paragraphs = [normal("intro")];

    let structured = interpret_structured(&cleaned, &paragraphs).expect("structured import");

    assert_eq!(structured.tables_imported, 1);
    assert!(matches!(structured.blocks.first(), Some(InputBlock::Paragraph(_))));
    let Some(InputBlock::Table(table)) = structured.blocks.get(1) else {
      panic!("expected a table block after the intro paragraph");
    };
    assert!(table.style.header_row);
    assert_eq!(
      table.column_widths,
      vec![InputTableColumnWidth::FixedPx(96), InputTableColumnWidth::FixedPx(192)]
    );
    assert_eq!(table.rows.len(), 1);
    assert_eq!(table.rows[0].cells.len(), 2);
    let InputTableCellBlock::Paragraph(left) = &table.rows[0].cells[0].blocks[0] else {
      panic!("expected a paragraph in the first cell");
    };
    assert_eq!(left.runs[0].text, "left");
  }

  #[test]
  fn vertical_merge_folds_continuation_into_row_span() {
    let body = r#"<w:tbl><w:tblGrid><w:gridCol w:w="1440"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:vMerge w:val="restart"/></w:tcPr><w:p><w:r><w:t>top</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:tcPr><w:vMerge/></w:tcPr><w:p/></w:tc></w:tr></w:tbl>"#;
    let cleaned = cleaned(body);

    let structured = interpret_structured(&cleaned, &[]).expect("structured import");

    let Some(InputBlock::Table(table)) = structured.blocks.first() else {
      panic!("expected a table block");
    };
    assert_eq!(table.rows.len(), 2);
    assert_eq!(table.rows[0].cells.len(), 1);
    assert_eq!(table.rows[0].cells[0].row_span, 2);
    assert!(table.rows[1].cells.is_empty());
  }

  #[test]
  fn inline_office_math_emits_equation_after_paragraph() {
    let body = r"<w:p><w:r><w:t>see</w:t></w:r><m:oMath><m:f><m:num><m:r><m:t>1</m:t></m:r></m:num><m:den><m:r><m:t>2</m:t></m:r></m:den></m:f></m:oMath></w:p>";
    let cleaned = cleaned(body);
    let paragraphs = [normal("see")];

    let structured = interpret_structured(&cleaned, &paragraphs).expect("structured import");

    assert_eq!(structured.equations_imported, 1);
    assert!(matches!(structured.blocks.first(), Some(InputBlock::Paragraph(_))));
    let Some(InputBlock::Equation(equation)) = structured.blocks.get(1) else {
      panic!("expected an equation block after the paragraph");
    };
    assert_eq!(equation.source, "\\frac{1}{2}");
  }

  #[test]
  fn body_without_objects_reports_zero_counts() {
    let cleaned = cleaned(r"<w:p><w:r><w:t>plain</w:t></w:r></w:p>");
    let structured = interpret_structured(&cleaned, &[normal("plain")]).expect("structured import");

    assert_eq!(structured.tables_imported, 0);
    assert_eq!(structured.images_imported, 0);
    assert_eq!(structured.equations_imported, 0);
    assert!(structured.assets.is_empty());
    assert_eq!(structured.blocks.len(), 1);
  }
}
