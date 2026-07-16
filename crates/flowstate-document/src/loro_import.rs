use std::{io, path::Path};

use flowstate_fidelity::{self as fidelity, FidelityClass};
use gpui_flowtext::{
  Block, BlockAlignment, DocumentParagraphInput, DocumentProjection, DocumentTheme, EquationDisplay, EquationSyntax, HighlightStyle, ImageBlock,
  ImageSizing, Paragraph, ParagraphStyle, RunSemanticStyle, RunStyles, TableBlock, TableCellBlock, TableColumnWidth, document_from_paragraphs,
  paragraph_text,
};
use loro::{ContainerTrait as _, LoroDoc, LoroMap, LoroMovableList, LoroResult, LoroText, LoroValue, TextDelta, ValueOrContainer, cursor::Side};
use rustc_hash::FxHashMap;
use uuid::Uuid;

use gpui_flowtext::{InputBlockAlignment, InputEquationDisplay, InputEquationSyntax, InputImageSizing, InputTableCellBlock};

use crate::{
  AssetChunk, BLOCKS_BY_ID, BODY_FLOW_ID, FLOW_ATTRS_KEY, FLOW_ID_KEY, FLOW_KIND_KEY, FLOW_TEXT_KEY, FLOWS_BY_ID, MARK_DIRECT_UNDERLINE,
  MARK_HIGHLIGHT_STYLE, MARK_PARAGRAPH_STYLE, MARK_RUN_SEMANTIC_STYLE, MARK_STRIKETHROUGH, MARK_VERT_ALIGN, OBJECT_REPLACEMENT,
  PARAGRAPHS_BY_ID, ROOT, ROOT_BODY_FLOW_ID, SECTIONS_BY_ID,
  loro_schema::{
    ASSETS_BY_ID, REVISIONS, SectionPageAttrs, TABLE_CELLS_BY_ID, TABLE_COLUMN_ORDER, TABLE_COLUMNS_BY_ID, TABLE_KEY, TABLE_ROW_ORDER,
    TABLE_ROWS_BY_ID, cell_flow_loro_id, cell_loro_id, column_loro_id, row_loro_id, write_section_page_attrs,
  },
};

/// Canonical result of an external import. The Loro document is authoritative;
/// the projection is a frontier-matched initial view built from the same semantic
/// import plan so the UI does not need to project the document a second time.
pub struct ImportedLoroDocument {
  pub doc: LoroDoc,
  pub projection: DocumentProjection,
}

pub fn import_document_projection(mut document: DocumentProjection, title: &str) -> io::Result<ImportedLoroDocument> {
  let doc = crate::loro_schema::new_loro_import_document(title).map_err(loro_io_error)?;
  if document.ids.document_id != 0 {
    crate::loro_schema::set_document_id(&doc, Uuid::from_u128(document.ids.document_id)).map_err(loro_io_error)?;
  }
  replace_body_from_document(&doc, &document).map_err(loro_io_error)?;
  import_assets(&doc, &document).map_err(loro_io_error)?;
  doc.commit();
  document.frontier = doc.state_frontiers().encode();
  fidelity_report_import(&doc, &document);
  Ok(ImportedLoroDocument { doc, projection: document })
}

/// §fidelity import completion event + seed invariant. The caller path always
/// reaches here after `replace_body_from_document`, so the checks read the freshly
/// imported canonical state. Gated on [`fidelity::enabled`]; strictly read-only
/// (no `ensure_*`, so a failing invariant never fabricates the structure it is
/// asserting). Emits the imported block-shape counts and asserts the document
/// seeds a leading sentinel newline plus at least one durable first-paragraph
/// metadata record.
fn fidelity_report_import(doc: &LoroDoc, document: &DocumentProjection) {
  if !fidelity::enabled() {
    return;
  }
  let paragraphs = document.paragraphs.len();
  let (mut blocks, mut tables, mut images, mut equations) = (0_usize, 0_usize, 0_usize, 0_usize);
  for block in document.blocks.iter() {
    blocks += 1;
    match block {
      Block::Table(_) => tables += 1,
      Block::Image(_) => images += 1,
      Block::Equation(_) => equations += 1,
      Block::Paragraph(_) => {},
    }
  }
  fidelity::event(FidelityClass::ImportExport, "import-complete", || {
    format!("paragraphs={paragraphs} blocks={blocks} tables={tables} images={images} equations={equations}")
  });
  let sentinel_ok = read_body_text(doc).is_some_and(|text| text.to_string().starts_with(crate::SENTINEL_NEWLINE));
  fidelity::check(sentinel_ok, FidelityClass::ImportExport, "missing-sentinel", || {
    "imported document body does not start with the sentinel newline".to_string()
  });
  let first_paragraph_ok = read_root_child_map(doc, PARAGRAPHS_BY_ID).is_some_and(|map| map.keys().next().is_some());
  fidelity::check(first_paragraph_ok, FidelityClass::ImportExport, "missing-first-paragraph", || {
    "imported document has no durable first-paragraph metadata record".to_string()
  });
}

/// Read-only resolution of a top-level `flowstate.root` child map. Unlike
/// `ensure_mergeable_map`, this never creates the container, so a fidelity
/// invariant cannot fabricate the very structure it is meant to verify.
fn read_root_child_map(doc: &LoroDoc, key: &str) -> Option<LoroMap> {
  match doc.get_map(ROOT).get(key)? {
    ValueOrContainer::Container(container) => container.into_map().ok(),
    ValueOrContainer::Value(_) => None,
  }
}

/// Read-only resolution of the body flow's text container (no `ensure_*`).
fn read_body_text(doc: &LoroDoc) -> Option<LoroText> {
  let flows = read_root_child_map(doc, FLOWS_BY_ID)?;
  let ValueOrContainer::Container(body) = flows.get(ROOT_BODY_FLOW_ID)? else {
    return None;
  };
  let body = body.into_map().ok()?;
  let ValueOrContainer::Container(text) = body.get(FLOW_TEXT_KEY)? else {
    return None;
  };
  text.into_text().ok()
}

pub fn import_paragraphs_as_loro(
  theme: DocumentTheme,
  paragraphs: Vec<DocumentParagraphInput>,
  title: &str,
) -> io::Result<ImportedLoroDocument> {
  import_document_projection(document_from_paragraphs(theme, paragraphs), title)
}

pub fn document_to_loro(document: &DocumentProjection, title: &str) -> io::Result<LoroDoc> {
  Ok(import_document_projection(document.clone(), title)?.doc)
}

pub fn write_imported_document_as_loro_db8(path: impl AsRef<Path>, document: &DocumentProjection, title: &str) -> io::Result<()> {
  let imported = import_document_projection(document.clone(), title)?;
  crate::DocumentPackage::from_loro_snapshot_with_assets(&imported.doc, title, assets_from_document(&imported.projection))?.write(path)
}

/// Flow architecture spec Part 2.1: (re)write ONE self-contained flow — a
/// debate-flow CELL's rich text — from a projection, via the exact body import
/// law ([`FlowTextImportPlan`]: one contiguous insert + merged mark ranges)
/// scoped to a single flow with its OWN paragraph registry. No block or
/// section records are written: single-flow block ids are derived from
/// paragraph ids at materialize time ([`crate::materialize_single_flow`]).
/// Existing flow text and registry records are replaced wholesale.
pub fn replace_single_flow_from_document(
  doc: &LoroDoc,
  flow: &LoroMap,
  registry: &LoroMap,
  flow_id: &str,
  document: &DocumentProjection,
) -> LoroResult<()> {
  let text = flow.ensure_mergeable_text(FLOW_TEXT_KEY)?;
  clear_map(registry)?;
  let plan = FlowTextImportPlan::for_document(document);
  let text_op_base = plan.write_to(Some(doc), &text)?;
  let mut paragraph_ix = 0_usize;
  for (block, position) in document.blocks.iter().zip(&plan.block_positions) {
    match (block, position) {
      (Block::Paragraph(_), FlowBlockPosition::Paragraph { boundary_pos, .. }) => {
        let paragraph_id = projection_paragraph_id(document, paragraph_ix);
        let record = registry.ensure_mergeable_map(&paragraph_id)?;
        record.insert("id", paragraph_id.as_str())?;
        record.insert("flow_id", flow_id)?;
        if let Some(cursor) = body_cursor_at(&text, text_op_base, *boundary_pos, Side::Left) {
          record.insert("start_cursor", cursor.encode())?;
        }
        if let Some(cursor) = body_cursor_at(&text, text_op_base, *boundary_pos, Side::Right) {
          record.insert("boundary_cursor", cursor.encode())?;
        }
        record.ensure_mergeable_map("attrs")?;
        paragraph_ix += 1;
      },
      _ => {
        return Err(loro::LoroError::ArgErr(
          format!("single flow `{flow_id}` cannot contain object blocks").into_boxed_str(),
        ));
      },
    }
  }
  Ok(())
}

pub(crate) fn replace_body_from_document(doc: &LoroDoc, document: &DocumentProjection) -> LoroResult<()> {
  let root = doc.get_map(ROOT);
  let flows = root.ensure_mergeable_map(FLOWS_BY_ID)?;
  let blocks = root.ensure_mergeable_map(BLOCKS_BY_ID)?;
  let paragraphs = root.ensure_mergeable_map(PARAGRAPHS_BY_ID)?;
  let sections = root.ensure_mergeable_map(SECTIONS_BY_ID)?;
  root.ensure_mergeable_list(REVISIONS)?;

  let body_flow = ensure_flow(&flows, ROOT_BODY_FLOW_ID, "body")?;
  let body_text = body_flow.ensure_mergeable_text(FLOW_TEXT_KEY)?;
  clear_map(&blocks)?;
  clear_map(&paragraphs)?;
  clear_map(&sections)?;

  let plan = hotpath::measure_block!("import_plan_build", FlowTextImportPlan::for_document(document));
  let body_text_op_base = hotpath::measure_block!("import_plan_write_body", plan.write_to(Some(doc), &body_text)?);

  let mut paragraph_ix = 0_usize;
  for (block_ix, (block, position)) in document
    .blocks
    .iter()
    .zip(&plan.block_positions)
    .enumerate()
  {
    match (block, position) {
      (Block::Paragraph(_), FlowBlockPosition::Paragraph { boundary_pos, .. }) => {
        import_paragraph_record(
          &paragraphs,
          &blocks,
          BODY_FLOW_ID,
          &body_text,
          body_text_op_base,
          *boundary_pos,
          projection_block_id(document, block_ix, "paragraph_block"),
          projection_paragraph_id(document, paragraph_ix),
        )?;
        paragraph_ix += 1;
      },
      (Block::Image(image), FlowBlockPosition::Object { anchor_pos }) => {
        import_image_block(
          &flows,
          &blocks,
          document,
          image,
          projection_block_id(document, block_ix, "image"),
          BODY_FLOW_ID,
          &body_text,
          body_text_op_base,
          *anchor_pos,
        )?;
      },
      (Block::Equation(equation), FlowBlockPosition::Object { anchor_pos }) => {
        let durable_block_id = projection_block_id(document, block_ix, "equation");
        let block = ensure_block(
          &blocks,
          durable_block_id.clone(),
          "equation",
          BODY_FLOW_ID,
          &body_text,
          body_text_op_base,
          *anchor_pos,
        )?;
        let source_flow_id = nested_flow_id("equation_source", &durable_block_id);
        block.insert("source_flow_id", source_flow_id.as_str())?;
        let source_flow = ensure_flow(&flows, &source_flow_id, "equation_source")?;
        replace_text(&source_flow.ensure_mergeable_text(FLOW_TEXT_KEY)?, equation.source.as_ref())?;
        let attrs = block.ensure_mergeable_map("attrs")?;
        attrs.insert("syntax", equation_syntax_name(equation.syntax))?;
        attrs.insert("display", equation_display_name(equation.display))?;
      },
      (Block::Table(table), FlowBlockPosition::Object { anchor_pos }) => {
        let durable_block_id = projection_block_id(document, block_ix, "table");
        let block = ensure_block(
          &blocks,
          durable_block_id.clone(),
          "table",
          BODY_FLOW_ID,
          &body_text,
          body_text_op_base,
          *anchor_pos,
        )?;
        import_table(&flows, &blocks, document, &block, table)?;
      },
      _ => unreachable!("flow import plan must preserve document block shape"),
    }
  }

  import_sections(document, &sections, &flows, &body_text, &plan.paragraphs)?;
  // §P2a: an import that carried no blocks still needs the canonical first
  // paragraph seed so the projector never has to fabricate an identity for the
  // lone sentinel boundary. A non-empty import already wrote its own
  // paragraph/block records, so only the empty case routes through the shared
  // seed (which is idempotent and converges with the runtime repair path).
  if document.blocks.is_empty() {
    crate::loro_schema::seed_document_body(doc)?;
  }
  Ok(())
}

fn import_image_block(
  flows: &LoroMap,
  blocks: &LoroMap,
  document: &DocumentProjection,
  image: &ImageBlock,
  durable_block_id: String,
  flow_id: &str,
  body_text: &LoroText,
  text_op_base: Option<(u64, i32)>,
  anchor_pos: usize,
) -> LoroResult<()> {
  let block = ensure_block(
    blocks,
    durable_block_id.clone(),
    "image",
    flow_id,
    body_text,
    text_op_base,
    anchor_pos,
  )?;
  block.insert("asset_id", image.asset_id.0.to_string())?;
  // §A11.9: a genuinely-LINKED image persists its external URL; the key is only
  // written when a non-empty URL exists (embedded images carry no key at all —
  // the presence-guarded delete keeps a re-imported block from resurrecting a
  // stale URL without minting tombstone ops on the common embedded path).
  match image
    .external_url
    .as_ref()
    .map(|url| -> &str { url.as_ref() })
    .filter(|url| !url.is_empty())
  {
    Some(url) => block.insert("external_url", url)?,
    None => {
      if block.get("external_url").is_some() {
        block.delete("external_url")?;
      }
    },
  }
  if let Some(asset) = document.assets.assets.get(&image.asset_id) {
    block.insert("content_hash", blake3::hash(&asset.bytes).to_hex().as_str())?;
    block.insert("mime_type", asset.mime_type.as_ref())?;
    block.insert("byte_length", i64::try_from(asset.bytes.len()).unwrap_or(i64::MAX))?;
  }
  let alt_text_flow_id = nested_flow_id("image_alt", &durable_block_id);
  block.insert("alt_text_flow_id", alt_text_flow_id.as_str())?;
  let alt_flow = ensure_flow(flows, &alt_text_flow_id, "alt_text")?;
  replace_text(&alt_flow.ensure_mergeable_text(FLOW_TEXT_KEY)?, image.alt_text.as_ref())?;
  let attrs = block.ensure_mergeable_map("attrs")?;
  attrs.insert("alignment", alignment_name(image.alignment))?;
  match image.sizing {
    ImageSizing::Intrinsic => attrs.insert("sizing", "intrinsic")?,
    ImageSizing::FitWidth => attrs.insert("sizing", "fit_width")?,
    ImageSizing::Fixed { width_px, height_px } => {
      attrs.insert("sizing", "fixed")?;
      attrs.insert("width_px", i64::from(width_px))?;
      if let Some(height_px) = height_px {
        attrs.insert("height_px", i64::from(height_px))?;
      }
    },
  };
  Ok(())
}

fn import_sections(
  document: &DocumentProjection,
  sections: &LoroMap,
  flows: &LoroMap,
  body_text: &LoroText,
  paragraph_plans: &[ParagraphTextImportPlan],
) -> LoroResult<()> {
  let paragraph_indexes = document
    .ids
    .paragraph_ids
    .iter()
    .enumerate()
    .map(|(ix, id)| (*id, ix))
    .collect::<rustc_hash::FxHashMap<_, _>>();
  for section in document.sections.iter() {
    let section_id = section.id.0.to_string();
    let section_map = sections.ensure_mergeable_map(&section_id)?;
    section_map.insert("id", section_id.as_str())?;
    section_map.insert("start_paragraph_id", section.start_paragraph.0.to_string())?;
    if let Some(parent_id) = section.parent_id {
      section_map.insert("parent_section_id", parent_id.0.to_string())?;
    }
    if let Some(heading_id) = section.heading_paragraph {
      section_map.insert("heading_paragraph_id", heading_id.0.to_string())?;
    }
    if let Some(end_id) = section.end_paragraph_exclusive {
      section_map.insert("end_paragraph_exclusive_id", end_id.0.to_string())?;
    }
    let gpui_flowtext::SectionKind::Custom(kind_slot) = section.kind;
    section_map.insert("kind_slot", i64::from(kind_slot))?;
    if let Some(paragraph_ix) = paragraph_indexes.get(&section.start_paragraph).copied()
      && let Some(boundary_pos) = paragraph_plans
        .get(paragraph_ix)
        .map(|paragraph| paragraph.boundary_pos)
      && let Some(cursor) = body_text.get_cursor(boundary_pos, Side::Left)
    {
      section_map.insert("start_cursor", cursor.encode())?;
    }
    let attrs = section_map.ensure_mergeable_map("attrs")?;
    attrs.insert("source", "paragraph_style_outline")?;
    // §11/§31: persist this section's page-structure attrs so they round-trip
    // losslessly through Loro. When the projection carries them on
    // `DocumentSection::page`, map them field-for-field onto the canonical Loro
    // mirror and write them (the writer also creates any referenced
    // header/footer flows via the `flows` map). Page-less sections fall back to
    // the documented defaults (US Letter, 1-inch margins, 1 column, portrait,
    // no numbering, no header/footer), matching the read path which always
    // projects `Some(..)`.
    let page_attrs = section
      .page
      .as_ref()
      .map_or_else(SectionPageAttrs::default, section_page_attrs_to_loro);
    write_section_page_attrs(&attrs, flows, &page_attrs)?;
  }
  Ok(())
}

/// §11: map a section's gpui-flowtext page-structure attrs onto the canonical
/// Loro mirror (`crate::loro_schema::SectionPageAttrs`) for the import write
/// path. This is the exact inverse of `loro_projection`'s
/// `project_section_page_attrs` read mapping; fully-qualified paths disambiguate
/// the structurally-identical `gpui_flowtext` and `crate::loro_schema` type
/// names. The owned header/footer flow id strings are cloned because the source
/// section is borrowed from the projection.
fn section_page_attrs_to_loro(page: &gpui_flowtext::SectionPageAttrs) -> SectionPageAttrs {
  SectionPageAttrs {
    page_size: crate::loro_schema::SectionPageSize {
      width_twips: page.page_size.width_twips,
      height_twips: page.page_size.height_twips,
    },
    margins: crate::loro_schema::SectionMargins {
      top_twips: page.margins.top_twips,
      right_twips: page.margins.right_twips,
      bottom_twips: page.margins.bottom_twips,
      left_twips: page.margins.left_twips,
    },
    columns: page.columns,
    orientation: match page.orientation {
      gpui_flowtext::SectionOrientation::Portrait => crate::loro_schema::SectionOrientation::Portrait,
      gpui_flowtext::SectionOrientation::Landscape => crate::loro_schema::SectionOrientation::Landscape,
    },
    page_numbering: crate::loro_schema::SectionPageNumbering {
      format: match page.page_numbering.format {
        gpui_flowtext::PageNumberFormat::None => crate::loro_schema::PageNumberFormat::None,
        gpui_flowtext::PageNumberFormat::Decimal => crate::loro_schema::PageNumberFormat::Decimal,
        gpui_flowtext::PageNumberFormat::LowerRoman => crate::loro_schema::PageNumberFormat::LowerRoman,
        gpui_flowtext::PageNumberFormat::UpperRoman => crate::loro_schema::PageNumberFormat::UpperRoman,
        gpui_flowtext::PageNumberFormat::LowerAlpha => crate::loro_schema::PageNumberFormat::LowerAlpha,
        gpui_flowtext::PageNumberFormat::UpperAlpha => crate::loro_schema::PageNumberFormat::UpperAlpha,
      },
      start: page.page_numbering.start,
    },
    header_flow_id: page.header_flow_id.clone(),
    footer_flow_id: page.footer_flow_id.clone(),
  }
}

#[hotpath::measure]
/// §act-twelve A12.3.2b: a body-text cursor by ARITHMETIC. The whole body is
/// ONE contiguous insert op, so the char at unicode `pos` has id
/// `(peer, base_counter + pos)` — no per-boundary chunk walk (`get_cursor`
/// was 2-3 walks per paragraph over a growing tree, the dominant CRDT-import
/// cost after the batched body write). `origin_pos` is not part of the
/// encoded form. Oracle: `FLOWSTATE_IMPORT_CURSOR_VERIFY=1` cross-checks
/// every constructed cursor against `get_cursor` (armed in the corpus
/// sweep).
fn body_cursor_at(text: &LoroText, base: Option<(u64, i32)>, pos: usize, side: Side) -> Option<loro::cursor::Cursor> {
  let constructed = base.map(|(peer, counter)| loro::cursor::Cursor::new(Some(loro::ID::new(peer, counter + pos as i32)), text.id(), side, pos));
  static VERIFY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
  let verify = *VERIFY.get_or_init(|| std::env::var_os("FLOWSTATE_IMPORT_CURSOR_VERIFY").is_some());
  if constructed.is_none() || verify {
    let walked = text.get_cursor(pos, side);
    if let (Some(constructed), Some(walked)) = (constructed.as_ref(), walked.as_ref()) {
      assert_eq!(
        constructed.encode(),
        walked.encode(),
        "import cursor arithmetic diverged from get_cursor at pos {pos} (side {side:?}): constructed={constructed:?} walked={walked:?}"
      );
    }
    if constructed.is_none() {
      return walked;
    }
  }
  constructed
}

fn import_paragraph_record(
  paragraphs: &LoroMap,
  blocks: &LoroMap,
  flow_id: &str,
  body_text: &LoroText,
  text_op_base: Option<(u64, i32)>,
  boundary_pos: usize,
  block_id: String,
  paragraph_id: String,
) -> LoroResult<()> {
  let paragraph_map = hotpath::measure_block!("import_record_maps", {
    let paragraph_map = paragraphs.ensure_mergeable_map(&paragraph_id)?;
    paragraph_map.insert("id", paragraph_id.as_str())?;
    paragraph_map.insert("flow_id", flow_id)?;
    paragraph_map
  });
  hotpath::measure_block!("import_record_cursors", {
    if let Some(cursor) = body_cursor_at(body_text, text_op_base, boundary_pos, Side::Left) {
      paragraph_map.insert("start_cursor", cursor.encode())?;
    }
    if let Some(cursor) = body_cursor_at(body_text, text_op_base, boundary_pos, Side::Right) {
      paragraph_map.insert("boundary_cursor", cursor.encode())?;
    }
  });
  // §perf-heaven T8.2: the `attrs` container is kept (the projection reads it and
  // the invalidation whitelist expects it), but the `*_container_id` MIRROR
  // values are dropped. They duplicated `map.id()` — write-only (no reader in the
  // projection or collab), yet each was a long flattened-cid `String` stored as a
  // map value AND minted as an op, ×5 per paragraph ×N paragraphs (a large slice
  // of the 38.7 KB/record). Derivable from the container itself if ever needed.
  paragraph_map.ensure_mergeable_map("attrs")?;
  ensure_block(blocks, block_id, "paragraph", flow_id, body_text, text_op_base, boundary_pos)?;
  Ok(())
}

#[derive(Clone, Copy, Debug)]
enum FlowBlockPosition {
  Paragraph { boundary_pos: usize },
  Object { anchor_pos: usize },
}

#[derive(Clone, Debug)]
struct ParagraphTextImportPlan {
  boundary_pos: usize,
}

#[derive(Clone, Debug)]
struct FlowTextImportPlan {
  delta: Vec<TextDelta>,
  unicode_len: usize,
  block_positions: Vec<FlowBlockPosition>,
  paragraphs: Vec<ParagraphTextImportPlan>,
}

impl FlowTextImportPlan {
  fn new(block_capacity: usize, delta_capacity: usize) -> Self {
    let mut delta = Vec::with_capacity(delta_capacity.max(block_capacity.saturating_add(1)));
    delta.push(TextDelta::Insert {
      insert: "\n".to_string(),
      attributes: Some(paragraph_style_attributes(ParagraphStyle::Normal)),
    });
    Self {
      delta,
      unicode_len: 1,
      block_positions: Vec::with_capacity(block_capacity),
      paragraphs: Vec::new(),
    }
  }

  fn for_document(document: &DocumentProjection) -> Self {
    let run_count = document
      .paragraphs
      .iter()
      .map(|paragraph| paragraph.runs.len())
      .sum::<usize>();
    let mut plan = Self::new(
      document.blocks.len(),
      run_count
        .saturating_add(document.blocks.len())
        .saturating_add(1),
    );
    let mut paragraph_ix = 0_usize;
    for block in document.blocks.iter() {
      match block {
        Block::Paragraph(paragraph) => {
          let paragraph_body = paragraph_text(document, paragraph_ix);
          plan.push_paragraph(paragraph, &paragraph_body);
          paragraph_ix += 1;
        },
        Block::Image(_) | Block::Equation(_) | Block::Table(_) => plan.push_object(),
      }
    }
    plan
  }

  fn push_paragraph(&mut self, paragraph: &Paragraph, paragraph_body: &str) {
    let boundary_pos = if self.block_positions.is_empty() {
      self.set_initial_paragraph_style(paragraph.style);
      0
    } else {
      let boundary_pos = self.unicode_len;
      push_rich_text_insert(&mut self.delta, "\n", Some(paragraph_style_attributes(paragraph.style)));
      self.unicode_len += 1;
      boundary_pos
    };

    self.push_paragraph_body(paragraph_body, &paragraph.runs);
    self
      .paragraphs
      .push(ParagraphTextImportPlan { boundary_pos });
    self
      .block_positions
      .push(FlowBlockPosition::Paragraph { boundary_pos });
  }

  fn set_initial_paragraph_style(&mut self, style: ParagraphStyle) {
    let Some(TextDelta::Insert { insert, attributes }) = self.delta.first_mut() else {
      unreachable!("flow import plan always starts with a sentinel newline");
    };
    debug_assert_eq!(insert, "\n");
    *attributes = Some(paragraph_style_attributes(style));
  }

  fn push_paragraph_body(&mut self, paragraph_body: &str, runs: &[gpui_flowtext::TextRun]) {
    let mut byte_offset = 0_usize;
    for run in runs {
      let byte_end = byte_offset.saturating_add(run.len);
      if byte_end > paragraph_body.len() || !paragraph_body.is_char_boundary(byte_offset) || !paragraph_body.is_char_boundary(byte_end) {
        break;
      }
      push_rich_text_insert(&mut self.delta, &paragraph_body[byte_offset..byte_end], run_style_attributes(run.styles));
      byte_offset = byte_end;
    }
    if byte_offset < paragraph_body.len() && paragraph_body.is_char_boundary(byte_offset) {
      push_rich_text_insert(&mut self.delta, &paragraph_body[byte_offset..], None);
    }
    self.unicode_len += paragraph_body.chars().count();
  }

  fn push_object(&mut self) {
    let anchor_pos = self.unicode_len;
    let object = OBJECT_REPLACEMENT.to_string();
    push_rich_text_insert(&mut self.delta, &object, None);
    self.unicode_len += 1;
    self
      .block_positions
      .push(FlowBlockPosition::Object { anchor_pos });
  }

  /// `doc` enables the batched mark path (needs the inner-doc handle); the
  /// tiny nested flows (captions, table cells) pass `None` and mark per run.
  /// Returns the `(peer, start_counter)` of the single contiguous body text
  /// op when a doc handle is supplied — every in-body position's cursor is
  /// then pure arithmetic off it (§act-twelve A12.3.2b).
  fn write_to(&self, doc: Option<&LoroDoc>, text: &LoroText) -> LoroResult<Option<(u64, i32)>> {
    let len = text.len_unicode();
    if len > 0 {
      text.delete(0, len)?;
    }
    // §perf (act three D.1/D.2): ONE contiguous insert op for the whole flow
    // text, then explicit merged style-mark ranges. The former `apply_delta`
    // minted one insert op (one op-id span) PER STYLE RUN — ~128k spans on
    // the reference doc — which fragmented every later whole-range operation
    // (a select-all delete became 128k delete ops, ~640 ms) and paid per-span
    // tree insertion at build time. Marks are anchor ops over the contiguous
    // text and do not fragment text ids; the end state (read back via
    // `to_delta`/the style tree) is identical because `apply_delta` itself
    // resolves attributes into `mark` calls after inserting.
    let mut full_text = String::with_capacity(
      self
        .delta
        .iter()
        .map(|span| span_text(span).map_or(0, str::len))
        .sum(),
    );
    for span in &self.delta {
      if let Some(insert) = span_text(span) {
        full_text.push_str(insert);
      }
    }
    // §act-twelve A12.3.2b: capture the (peer, counter) the contiguous insert
    // starts at — pending-txn ops extend the peer's oplog counter linearly.
    let text_op_base = doc.map(|doc| {
      let peer = doc.peer_id();
      // `oplog_vv` already reflects ops applied inside the open transaction,
      // so the next op's counter is exactly the vv entry (verified against
      // `get_cursor` by the FLOWSTATE_IMPORT_CURSOR_VERIFY oracle).
      (peer, doc.oplog_vv().get(&peer).copied().unwrap_or(0))
    });
    static POP_PROBE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let pop_probe = *POP_PROBE.get_or_init(|| std::env::var_os("FLOWSTATE_POPULATE_PROBE").is_some());
    let probe_t = std::time::Instant::now();
    hotpath::measure_block!("import_body_single_insert", text.insert(0, &full_text)?);
    if pop_probe {
      eprintln!(
        "[flowstate-populate-probe] insert={:?} chars={} spans={}",
        probe_t.elapsed(),
        full_text.chars().count(),
        self.delta.len()
      );
    }
    let probe_t = std::time::Instant::now();

    // Per-key run merging: adjacent spans that share one (key, value) pair
    // become a single mark op even when their full attribute maps differ.
    let mut runs_by_key: FxHashMap<&str, Vec<MarkRun>> = FxHashMap::default();
    let mut unicode_pos = 0usize;
    for span in &self.delta {
      let TextDelta::Insert { insert, attributes } = span else {
        continue;
      };
      let span_len = insert.chars().count();
      if let Some(attributes) = attributes {
        for (key, value) in attributes {
          let runs = runs_by_key.entry(key.as_str()).or_default();
          match runs.last_mut() {
            Some(last) if last.end == unicode_pos && last.value == *value => last.end = unicode_pos + span_len,
            _ => runs.push(MarkRun {
              start: unicode_pos,
              end: unicode_pos + span_len,
              value: value.clone(),
            }),
          }
        }
      }
      unicode_pos += span_len;
    }
    hotpath::measure_block!("import_body_marks", {
      match doc {
        // Batched mark application (vendored `mark_batch`): one transaction
        // ceremony for the whole style set instead of two per mark — the mark
        // pass dominated the body write (~730 ms) once the text became a
        // single insert.
        Some(doc) => {
          let mut marks: Vec<(usize, usize, loro::InternalString, LoroValue)> = Vec::new();
          for (key, runs) in runs_by_key {
            for run in runs {
              marks.push((run.start, run.end, key.into(), run.value));
            }
          }
          if pop_probe {
            eprintln!("[flowstate-populate-probe] mark_count={}", marks.len());
          }
          doc
            .inner()
            .get_text(text.id())
            .mark_batch(marks, loro::cursor::PosType::Unicode)?;
        },
        None => {
          for (key, runs) in runs_by_key {
            for run in runs {
              text.mark(run.start..run.end, key, run.value)?;
            }
          }
        },
      }
    });
    if pop_probe {
      eprintln!("[flowstate-populate-probe] marks={:?}", probe_t.elapsed());
    }
    Ok(text_op_base)
  }
}

struct MarkRun {
  start: usize,
  end: usize,
  value: LoroValue,
}

fn span_text(span: &TextDelta) -> Option<&str> {
  match span {
    TextDelta::Insert { insert, .. } => Some(insert.as_str()),
    _ => None,
  }
}

fn paragraph_style_attributes(style: ParagraphStyle) -> FxHashMap<String, LoroValue> {
  let mut attributes = FxHashMap::default();
  attributes.insert(MARK_PARAGRAPH_STYLE.to_string(), paragraph_style_value(style).into());
  attributes
}

pub fn run_style_attributes(styles: RunStyles) -> Option<FxHashMap<String, LoroValue>> {
  let mut attributes = FxHashMap::default();
  if let RunSemanticStyle::Custom(slot) = styles.semantic {
    attributes.insert(MARK_RUN_SEMANTIC_STYLE.to_string(), i64::from(slot).into());
  }
  if let Some(HighlightStyle::Custom(slot)) = styles.highlight {
    attributes.insert(MARK_HIGHLIGHT_STYLE.to_string(), i64::from(slot).into());
  }
  if styles.direct_underline {
    attributes.insert(MARK_DIRECT_UNDERLINE.to_string(), true.into());
  }
  if styles.strikethrough {
    attributes.insert(MARK_STRIKETHROUGH.to_string(), true.into());
  }
  if let Some(value) = styles.vert_align.mark_value() {
    attributes.insert(MARK_VERT_ALIGN.to_string(), value.into());
  }
  (!attributes.is_empty()).then_some(attributes)
}

fn push_rich_text_insert(delta: &mut Vec<TextDelta>, value: &str, attributes: Option<FxHashMap<String, LoroValue>>) {
  if value.is_empty() {
    return;
  }
  if let Some(TextDelta::Insert {
    insert,
    attributes: previous_attributes,
  }) = delta.last_mut()
    && previous_attributes.as_ref() == attributes.as_ref()
  {
    insert.push_str(value);
    return;
  }
  delta.push(TextDelta::Insert {
    insert: value.to_string(),
    attributes,
  });
}

fn import_table(
  flows: &LoroMap,
  blocks: &LoroMap,
  document: &DocumentProjection,
  block: &LoroMap,
  table: &TableBlock,
) -> LoroResult<()> {
  let table_map = block.ensure_mergeable_map(TABLE_KEY)?;
  let row_order = table_map.ensure_mergeable_movable_list(TABLE_ROW_ORDER)?;
  let column_order = table_map.ensure_mergeable_movable_list(TABLE_COLUMN_ORDER)?;
  let rows_by_id = table_map.ensure_mergeable_map(TABLE_ROWS_BY_ID)?;
  let columns_by_id = table_map.ensure_mergeable_map(TABLE_COLUMNS_BY_ID)?;
  let cells_by_id = table_map.ensure_mergeable_map(TABLE_CELLS_BY_ID)?;
  table_map.insert("columns_container_id", columns_by_id.id().to_string())?;
  table_map.insert("cells_container_id", cells_by_id.id().to_string())?;
  table_map.insert("header_row", table.style.header_row)?;

  // §P2b create-only: import builds a brand-new table, but it uses the SAME
  // durable-id `ensure` scheme as the incremental edit path (never clear/rekey),
  // so row/column/cell identity is stable and concurrent creation of the same id
  // merges (LWW) instead of duplicating.
  for column in &table.columns {
    let column_id = column_loro_id(column.id);
    column_order.push(column_id.as_str())?;
    let column_map = columns_by_id.ensure_mergeable_map(&column_id)?;
    column_map.insert("id", column_id.as_str())?;
    let _attrs = column_map.ensure_mergeable_map("attrs")?;
    match column.width {
      TableColumnWidth::Auto => column_map.insert("width_kind", "auto")?,
      TableColumnWidth::FixedPx(px) => {
        column_map.insert("width_kind", "fixed_px")?;
        column_map.insert("width_px", i64::from(px))?;
      },
      TableColumnWidth::Fraction(fraction) => {
        column_map.insert("width_kind", "fraction")?;
        column_map.insert("fraction", i64::from(fraction))?;
      },
    };
  }

  for row in &table.rows {
    let row_id = row_loro_id(row.id);
    row_order.push(row_id.as_str())?;
    let row_map = rows_by_id.ensure_mergeable_map(&row_id)?;
    row_map.insert("id", row_id.as_str())?;
    let _attrs = row_map.ensure_mergeable_map("attrs")?;
    for cell in &row.cells {
      let cell_id = cell_loro_id(cell.id);
      let column_id = column_loro_id(cell.column_id);
      let cell_row_id = row_loro_id(cell.row_id);
      let cell_map = cells_by_id.ensure_mergeable_map(&cell_id)?;
      cell_map.insert("id", cell_id.as_str())?;
      cell_map.insert("row_id", cell_row_id.as_str())?;
      cell_map.insert("column_id", column_id.as_str())?;
      cell_map.insert("row_span", i64::from(cell.row_span))?;
      cell_map.insert("column_span", i64::from(cell.col_span))?;
      let _attrs = cell_map.ensure_mergeable_map("attrs")?;
      let flow_id = cell_flow_loro_id(&cell_id);
      cell_map.insert("flow_id", flow_id.as_str())?;
      let nested_table_ids = cell_map.ensure_mergeable_movable_list("nested_table_ids")?;
      let nested_tables_by_id = cell_map.ensure_mergeable_map("nested_tables_by_id")?;
      clear_movable_list(&nested_table_ids)?;
      clear_map(&nested_tables_by_id)?;
      let flow = ensure_flow(flows, &flow_id, "table_cell")?;
      let text = flow.ensure_mergeable_text(FLOW_TEXT_KEY)?;
      cell_map.insert("text_container_id", text.id().to_string())?;
      let cell_delta_capacity = cell
        .blocks
        .iter()
        .map(|block| match block {
          TableCellBlock::Paragraph(paragraph) => paragraph.paragraph.runs.len().saturating_add(1),
          TableCellBlock::Table(_) | TableCellBlock::Image(_) | TableCellBlock::Equation(_) => 1,
        })
        .sum();
      let mut cell_plan = FlowTextImportPlan::new(cell.blocks.len(), cell_delta_capacity);
      for cell_block in &cell.blocks {
        match cell_block {
          TableCellBlock::Paragraph(paragraph) => cell_plan.push_paragraph(&paragraph.paragraph, &paragraph.text),
          TableCellBlock::Table(_) | TableCellBlock::Image(_) | TableCellBlock::Equation(_) => cell_plan.push_object(),
        }
      }
      cell_plan.write_to(None, &text)?;
      for (block_ix, (cell_block, position)) in cell
        .blocks
        .iter()
        .zip(&cell_plan.block_positions)
        .enumerate()
      {
        let FlowBlockPosition::Object { anchor_pos } = position else {
          continue;
        };
        match cell_block {
          TableCellBlock::Table(nested) => {
            let nested_table_id = format!("{cell_id}.nested_table.{block_ix}");
            nested_table_ids.push(nested_table_id.as_str())?;
            let nested_map = nested_tables_by_id.ensure_mergeable_map(&nested_table_id)?;
            nested_map.insert("id", nested_table_id.as_str())?;
            nested_map.insert("kind", "table")?;
            if let Some(cursor) = text.get_cursor(*anchor_pos, Side::Left) {
              nested_map.insert("anchor_cursor", cursor.encode())?;
            }
            nested_map.ensure_mergeable_map("attrs")?;
            import_table(flows, blocks, document, &nested_map, nested)?;
          },
          // B-S5: cell objects live in the GLOBAL block registry, anchored in
          // the cell's flow — the same record shape the body uses, so
          // `object_blocks_for_flow` (flow-filtered) materializes them and the
          // quarantine/defect law applies unchanged.
          TableCellBlock::Image(image) => {
            let durable_block_id = format!("{cell_id}.object.{block_ix}");
            import_image_block(flows, blocks, document, image, durable_block_id, &flow_id, &text, None, *anchor_pos)?;
          },
          TableCellBlock::Equation(equation) => {
            let durable_block_id = format!("{cell_id}.object.{block_ix}");
            let block = ensure_block(blocks, durable_block_id.clone(), "equation", &flow_id, &text, None, *anchor_pos)?;
            let source_flow_id = nested_flow_id("equation_source", &durable_block_id);
            block.insert("source_flow_id", source_flow_id.as_str())?;
            let source_flow = ensure_flow(flows, &source_flow_id, "equation_source")?;
            replace_text(&source_flow.ensure_mergeable_text(FLOW_TEXT_KEY)?, equation.source.as_ref())?;
            let attrs = block.ensure_mergeable_map("attrs")?;
            attrs.insert("syntax", equation_syntax_name(equation.syntax))?;
            attrs.insert("display", equation_display_name(equation.display))?;
          },
          TableCellBlock::Paragraph(_) => {},
        }
      }
    }
  }
  Ok(())
}

pub fn import_assets(doc: &LoroDoc, document: &DocumentProjection) -> LoroResult<()> {
  let root = doc.get_map(ROOT);
  let assets = root.ensure_mergeable_map(ASSETS_BY_ID)?;
  clear_map(&assets)?;
  for asset in document.assets.assets.values() {
    let asset_id = asset.asset_id_string();
    let asset_map = assets.ensure_mergeable_map(&asset_id)?;
    let hash = blake3::hash(&asset.bytes);
    asset_map.insert("asset_id", asset_id.as_str())?;
    asset_map.insert("content_hash", hash.to_hex().as_str())?;
    asset_map.insert("mime_type", asset.mime_type.as_ref())?;
    asset_map.insert("byte_length", i64::try_from(asset.bytes.len()).unwrap_or(i64::MAX))?;
    if let Some((width, height)) = image_dimensions(asset.mime_type.as_ref(), &asset.bytes) {
      // §31/§14: canonical AssetMap dimensions, stored as two i64 keys when derivable.
      asset_map.insert("dimension_width", width)?;
      asset_map.insert("dimension_height", height)?;
    }
    if let Some(original_name) = &asset.original_name {
      asset_map.insert("original_name", original_name.as_ref())?;
    }
  }
  Ok(())
}

/// Derive an image asset's intrinsic pixel dimensions from its bytes.
///
/// Returns `None` for non-image assets or when the size cannot be determined,
/// in which case the asset map simply omits its `dimension_*` keys (§14).
fn image_dimensions(mime_type: &str, bytes: &[u8]) -> Option<(i64, i64)> {
  if !mime_type.starts_with("image/") {
    return None;
  }
  let size = imagesize::blob_size(bytes).ok()?;
  Some((i64::try_from(size.width).ok()?, i64::try_from(size.height).ok()?))
}

pub fn assets_from_document(document: &DocumentProjection) -> Vec<AssetChunk> {
  document
    .assets
    .assets
    .values()
    .map(|asset| AssetChunk {
      asset_id: asset.id.0,
      content_hash: *blake3::hash(&asset.bytes).as_bytes(),
      mime_type: asset.mime_type.to_string(),
      byte_length: asset.bytes.len() as u64,
      bytes: Vec::clone(&asset.bytes),
      metadata: Vec::new(),
    })
    .collect()
}

trait AssetRecordExt {
  fn asset_id_string(&self) -> String;
}

impl AssetRecordExt for gpui_flowtext::AssetRecord {
  fn asset_id_string(&self) -> String {
    self.id.0.to_string()
  }
}

fn ensure_flow(flows: &LoroMap, flow_id: &str, kind: &str) -> LoroResult<LoroMap> {
  let flow = flows.ensure_mergeable_map(flow_id)?;
  flow.insert(FLOW_ID_KEY, flow_id)?;
  flow.insert(FLOW_KIND_KEY, kind)?;
  let text = flow.ensure_mergeable_text(FLOW_TEXT_KEY)?;
  let _attrs = flow.ensure_mergeable_map(FLOW_ATTRS_KEY)?;
  flow.insert("text_container_id", text.id().to_string())?;
  Ok(flow)
}

fn ensure_block(
  blocks: &LoroMap,
  block_id: String,
  kind: &str,
  flow_id: &str,
  text: &LoroText,
  text_op_base: Option<(u64, i32)>,
  pos: usize,
) -> LoroResult<LoroMap> {
  let block = blocks.ensure_mergeable_map(&block_id)?;
  block.insert("id", block_id.as_str())?;
  block.insert("kind", kind)?;
  block.insert("flow_id", flow_id)?;
  if let Some(cursor) = body_cursor_at(text, text_op_base, pos, Side::Left) {
    block.insert("anchor_cursor", cursor.encode())?;
  }
  // §perf-heaven T8.2: keep the `attrs`/`nested_refs` containers (read + whitelisted),
  // drop the write-only `*_container_id` mirror strings (duplicated `map.id()`).
  block.ensure_mergeable_map("attrs")?;
  block.ensure_mergeable_map("nested_refs")?;
  Ok(block)
}

fn replace_text(text: &LoroText, value: &str) -> LoroResult<()> {
  let len = text.len_unicode();
  if len > 0 {
    text.delete(0, len)?;
  }
  if !value.is_empty() {
    text.insert(0, value)?;
  }
  Ok(())
}

fn clear_map(map: &LoroMap) -> LoroResult<()> {
  // §29: use Loro's native container clear instead of deleting keys one-by-one.
  map.clear()
}

fn clear_movable_list(list: &LoroMovableList) -> LoroResult<()> {
  list.clear()
}

fn paragraph_style_value(style: ParagraphStyle) -> i64 {
  match style {
    ParagraphStyle::Normal => 0,
    ParagraphStyle::Custom(slot) => i64::from(slot) + 1,
  }
}

fn projection_block_id(document: &DocumentProjection, block_ix: usize, kind: &str) -> String {
  document
    .ids
    .block_ids
    .get(block_ix)
    .map_or_else(|| fallback_id(kind, block_ix), |id| format!("{kind}.{}", id.0))
}

fn projection_paragraph_id(document: &DocumentProjection, paragraph_ix: usize) -> String {
  document
    .ids
    .paragraph_ids
    .get(paragraph_ix)
    .map_or_else(|| fallback_id("paragraph", paragraph_ix), |id| format!("paragraph.{}", id.0))
}

fn fallback_id(kind: &str, ix: usize) -> String {
  format!(
    "{kind}.{ix}.{}",
    Uuid::new_v5(&Uuid::NAMESPACE_OID, format!("{kind}.{ix}").as_bytes()).as_u128()
  )
}

fn nested_flow_id(kind: &str, block_id: &str) -> String {
  format!("{block_id}.{kind}")
}

fn alignment_name(alignment: BlockAlignment) -> &'static str {
  match alignment {
    BlockAlignment::Left => "left",
    BlockAlignment::Center => "center",
    BlockAlignment::Right => "right",
  }
}

/// B-S5: one cell-object record (image/equation) into the GLOBAL registry,
/// anchored in the cell flow. The runtime's whole-cell writer shares the
/// import path's record shape, so `object_blocks_for_flow` materializes both
/// origins identically. Returns `false` for non-object blocks.
pub fn write_cell_object_record(
  doc: &LoroDoc,
  cell_id: &str,
  block_ix: usize,
  flow_id: &str,
  text: &LoroText,
  anchor_pos: usize,
  object: &InputTableCellBlock,
) -> LoroResult<bool> {
  let root = doc.get_map(ROOT);
  let flows = root.ensure_mergeable_map(FLOWS_BY_ID)?;
  let blocks = root.ensure_mergeable_map(BLOCKS_BY_ID)?;
  let durable_block_id = format!("{cell_id}.object.{block_ix}");
  match object {
    InputTableCellBlock::Image(image) => {
      let block = ensure_block(&blocks, durable_block_id.clone(), "image", flow_id, text, None, anchor_pos)?;
      block.insert("asset_id", image.asset_id.0.to_string())?;
      match image.external_url.as_deref().filter(|url| !url.is_empty()) {
        Some(url) => block.insert("external_url", url)?,
        None => {
          if block.get("external_url").is_some() {
            block.delete("external_url")?;
          }
        },
      }
      let alt_text_flow_id = nested_flow_id("image_alt", &durable_block_id);
      block.insert("alt_text_flow_id", alt_text_flow_id.as_str())?;
      let alt_flow = ensure_flow(&flows, &alt_text_flow_id, "alt_text")?;
      replace_text(&alt_flow.ensure_mergeable_text(FLOW_TEXT_KEY)?, &image.alt_text)?;
      let attrs = block.ensure_mergeable_map("attrs")?;
      attrs.insert(
        "alignment",
        match image.alignment {
          InputBlockAlignment::Left => "left",
          InputBlockAlignment::Center => "center",
          InputBlockAlignment::Right => "right",
        },
      )?;
      match image.sizing {
        InputImageSizing::Intrinsic => attrs.insert("sizing", "intrinsic")?,
        InputImageSizing::FitWidth => attrs.insert("sizing", "fit_width")?,
        InputImageSizing::Fixed { width_px, height_px } => {
          attrs.insert("sizing", "fixed")?;
          attrs.insert("width_px", i64::from(width_px))?;
          if let Some(height_px) = height_px {
            attrs.insert("height_px", i64::from(height_px))?;
          }
        },
      };
      Ok(true)
    },
    InputTableCellBlock::Equation(equation) => {
      let block = ensure_block(&blocks, durable_block_id.clone(), "equation", flow_id, text, None, anchor_pos)?;
      let source_flow_id = nested_flow_id("equation_source", &durable_block_id);
      block.insert("source_flow_id", source_flow_id.as_str())?;
      let source_flow = ensure_flow(&flows, &source_flow_id, "equation_source")?;
      replace_text(&source_flow.ensure_mergeable_text(FLOW_TEXT_KEY)?, &equation.source)?;
      let attrs = block.ensure_mergeable_map("attrs")?;
      attrs.insert(
        "syntax",
        match equation.syntax {
          InputEquationSyntax::Latex => "latex",
        },
      )?;
      attrs.insert(
        "display",
        match equation.display {
          InputEquationDisplay::Display => "display",
          InputEquationDisplay::InlineLikeParagraph => "inline_like_paragraph",
        },
      )?;
      Ok(true)
    },
    InputTableCellBlock::Paragraph(_) | InputTableCellBlock::Table(_) => Ok(false),
  }
}

fn equation_syntax_name(syntax: EquationSyntax) -> &'static str {
  match syntax {
    EquationSyntax::Latex => "latex",
  }
}

fn equation_display_name(display: EquationDisplay) -> &'static str {
  match display {
    EquationDisplay::Display => "display",
    EquationDisplay::InlineLikeParagraph => "inline_like_paragraph",
  }
}

fn loro_io_error(error: impl std::error::Error + Send + Sync + 'static) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, error)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn projection_identity_round_trips_into_loro() -> io::Result<()> {
    let mut source = gpui_flowtext::document_from_input_blocks(
      crate::flowstate_document_theme(),
      vec![gpui_flowtext::InputBlock::Paragraph(gpui_flowtext::InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![gpui_flowtext::InputRun {
          text: "identity".to_string(),
          styles: RunStyles::default(),
        }],
      })],
    );
    source.ids.document_id = 0x0123;
    std::sync::Arc::make_mut(&mut source.ids.paragraph_ids)[0] = gpui_flowtext::ParagraphId(0x0456);
    std::sync::Arc::make_mut(&mut source.ids.block_ids)[0] = gpui_flowtext::BlockId(0x0789);

    let doc = document_to_loro(&source, "Identity")?;
    let projected = crate::document_from_loro(&doc)?;

    assert_eq!(projected.ids.document_id, source.ids.document_id);
    assert_eq!(projected.ids.paragraph_ids, source.ids.paragraph_ids);
    assert_eq!(projected.ids.block_ids, source.ids.block_ids);
    Ok(())
  }

  #[test]
  fn custom_paragraph_style_slot_zero_round_trips() -> io::Result<()> {
    let source = gpui_flowtext::document_from_input_blocks(
      crate::flowstate_document_theme(),
      vec![gpui_flowtext::InputBlock::Paragraph(gpui_flowtext::InputParagraph {
        style: ParagraphStyle::Custom(0),
        runs: vec![gpui_flowtext::InputRun {
          text: "pocket".to_string(),
          styles: RunStyles::default(),
        }],
      })],
    );

    let doc = document_to_loro(&source, "Pocket")?;
    let projected = crate::document_from_loro(&doc)?;
    assert_eq!(projected.paragraphs[0].style, ParagraphStyle::Custom(0));
    Ok(())
  }

  #[test]
  fn bulk_import_preserves_empty_paragraphs_unicode_and_frontier() -> io::Result<()> {
    let paragraphs = vec![
      DocumentParagraphInput {
        style: ParagraphStyle::Custom(0),
        runs: Vec::new(),
      },
      DocumentParagraphInput {
        style: ParagraphStyle::Normal,
        runs: Vec::new(),
      },
      DocumentParagraphInput {
        style: ParagraphStyle::Custom(2),
        runs: vec![gpui_flowtext::DocumentRunInput {
          text: "héllo 世界".to_string(),
          styles: RunStyles {
            semantic: RunSemanticStyle::Custom(3),
            direct_underline: true,
            ..RunStyles::default()
          },
        }],
      },
    ];

    let imported = import_paragraphs_as_loro(DocumentTheme::default(), paragraphs, "Bulk import")?;
    assert_eq!(imported.projection.frontier, imported.doc.state_frontiers().encode());
    assert_eq!(crate::loro_schema::body_text(&imported.doc).to_string(), "\n\n\nhéllo 世界");

    let projected = crate::document_from_loro(&imported.doc)?;
    assert_eq!(projected.paragraphs.len(), 3);
    assert_eq!(paragraph_text(&projected, 0), "");
    assert_eq!(paragraph_text(&projected, 1), "");
    assert_eq!(paragraph_text(&projected, 2), "héllo 世界");
    assert_eq!(projected.paragraphs[0].style, ParagraphStyle::Custom(0));
    assert_eq!(projected.paragraphs[2].style, ParagraphStyle::Custom(2));
    assert_eq!(projected.paragraphs[2].runs[0].styles.semantic, RunSemanticStyle::Custom(3));
    assert!(projected.paragraphs[2].runs[0].styles.direct_underline);
    Ok(())
  }

  #[test]
  fn bulk_delta_import_preserves_all_semantic_run_attributes() -> io::Result<()> {
    let expected = RunStyles {
      semantic: RunSemanticStyle::Custom(3),
      direct_underline: true,
      strikethrough: true,
      highlight: Some(HighlightStyle::Custom(4)),
      // Superscript rides the same import→mark→projection round-trip as the other
      // orthogonal run attributes; the `== expected` assertion below verifies it.
      vert_align: gpui_flowtext::VertAlign::Superscript,
    };
    let imported = import_paragraphs_as_loro(
      crate::flowstate_document_theme(),
      vec![DocumentParagraphInput {
        style: ParagraphStyle::Custom(2),
        runs: vec![
          gpui_flowtext::DocumentRunInput {
            text: "styled".to_string(),
            styles: expected,
          },
          gpui_flowtext::DocumentRunInput {
            text: " plain".to_string(),
            styles: RunStyles::default(),
          },
        ],
      }],
      "Rich delta import",
    )?;

    let projected = crate::document_from_loro(&imported.doc)?;
    assert_eq!(paragraph_text(&projected, 0), "styled plain");
    assert_eq!(projected.paragraphs[0].style, ParagraphStyle::Custom(2));
    assert!(
      projected.paragraphs[0]
        .runs
        .iter()
        .any(|run| run.styles == expected)
    );
    Ok(())
  }

  #[test]
  fn bulk_import_handles_large_paragraph_sets_without_reprojection() -> io::Result<()> {
    let paragraphs = (0..2_000)
      .map(|ix| DocumentParagraphInput {
        style: if ix % 11 == 0 {
          ParagraphStyle::Custom(1)
        } else {
          ParagraphStyle::Normal
        },
        runs: vec![gpui_flowtext::DocumentRunInput {
          text: format!("paragraph {ix}"),
          styles: RunStyles::default(),
        }],
      })
      .collect();

    let imported = import_paragraphs_as_loro(DocumentTheme::default(), paragraphs, "Large import")?;
    assert_eq!(imported.projection.paragraphs.len(), 2_000);
    assert_eq!(imported.projection.frontier, imported.doc.state_frontiers().encode());
    assert_eq!(paragraph_text(&imported.projection, 1_999), "paragraph 1999");
    Ok(())
  }

  #[test]
  fn large_document_materializes_without_quadratic_boundary_scan() -> io::Result<()> {
    // Regression for the large-document collab/editor hang. Unlike the import path
    // above (which reuses its freshly-built projection), `document_from_loro`
    // RE-derives the projection from the canonical doc, resolving a durable
    // paragraph + paragraph-block id for every boundary. That resolution formerly
    // rescanned every paragraph/block record per boundary and materialized the
    // whole body string per record — O(paragraphs²·chars) — which pegged the CRDT
    // actor thread at 100% CPU and never returned. With the one-pass boundary index
    // it is ~linear; this test projecting a 2,000-paragraph doc to completion is the
    // guard (reintroducing the quadratic would hang the suite), and matching the
    // import projection's ids proves the index selects the same id the per-boundary
    // scan did, at scale.
    let paragraphs = (0..2_000)
      .map(|ix| DocumentParagraphInput {
        style: if ix % 11 == 0 {
          ParagraphStyle::Custom(1)
        } else {
          ParagraphStyle::Normal
        },
        runs: vec![gpui_flowtext::DocumentRunInput {
          text: format!("paragraph {ix}"),
          styles: RunStyles::default(),
        }],
      })
      .collect();
    let imported = import_paragraphs_as_loro(DocumentTheme::default(), paragraphs, "Large projection")?;

    let projected = crate::loro_projection::document_from_loro(&imported.doc)?;

    assert_eq!(projected.paragraphs.len(), 2_000);
    assert_eq!(paragraph_text(&projected, 1_999), "paragraph 1999");
    // Re-derived ids match the import projection's ids (the one-pass index resolves
    // every boundary to the identical durable id the old per-boundary scan did)...
    assert_eq!(projected.ids.paragraph_ids, imported.projection.ids.paragraph_ids);
    assert_eq!(projected.ids.block_ids, imported.projection.ids.block_ids);
    // ...and every boundary resolved to a distinct durable id (no collisions, no
    // fabricated fallbacks from a mis-keyed index).
    let distinct_paragraph_ids = projected
      .ids
      .paragraph_ids
      .iter()
      .map(|id| id.0)
      .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
      distinct_paragraph_ids.len(),
      2_000,
      "every boundary must resolve to a unique paragraph id"
    );
    Ok(())
  }

  #[test]
  fn section_page_attrs_round_trip_through_import_write_path() -> io::Result<()> {
    let mut source = gpui_flowtext::document_from_input_blocks(
      crate::flowstate_document_theme(),
      vec![gpui_flowtext::InputBlock::Paragraph(gpui_flowtext::InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![gpui_flowtext::InputRun {
          text: "section body".to_string(),
          styles: RunStyles::default(),
        }],
      })],
    );
    // §11: non-default page structure on the source section must survive the
    // import write path and be projected back identically.
    let page = gpui_flowtext::SectionPageAttrs {
      page_size: gpui_flowtext::SectionPageSize {
        width_twips: 15_840,
        height_twips: 12_240,
      },
      margins: gpui_flowtext::SectionMargins {
        top_twips: 720,
        right_twips: 720,
        bottom_twips: 1_440,
        left_twips: 1_440,
      },
      columns: 2,
      orientation: gpui_flowtext::SectionOrientation::Landscape,
      page_numbering: gpui_flowtext::SectionPageNumbering {
        format: gpui_flowtext::PageNumberFormat::LowerRoman,
        start: 3,
      },
      header_flow_id: Some("section.s1.header".to_string()),
      footer_flow_id: Some("section.s1.footer".to_string()),
    };
    source.sections = std::sync::Arc::new(vec![gpui_flowtext::DocumentSection {
      id: gpui_flowtext::SectionId(0x5ec1),
      parent_id: None,
      kind: gpui_flowtext::SectionKind::Custom(0),
      heading_paragraph: None,
      start_paragraph: source.ids.paragraph_ids[0],
      end_paragraph_exclusive: None,
      page: Some(page.clone()),
    }]);

    let doc = document_to_loro(&source, "Section page attrs")?;
    let projected = crate::document_from_loro(&doc)?;

    assert_eq!(projected.sections.len(), 1);
    assert_eq!(projected.sections[0].page, Some(page));
    Ok(())
  }
}
