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
//!
//! §act-nine A9.2: the walk consumes the ALREADY-PARSED [`CT_Document`] produced
//! by the direct-properties pass instead of building its own owned [`XmlNode`]
//! tree (that was the third heavy `document.xml` parse per import). The typed
//! model captures unknown wrappers (`mc:AlternateContent`, inline `w:sdt`,
//! `w:smartTag`, tracked changes, ...) as raw byte chunks rather than typed
//! content, so a cheap parity pre-scan ([`typed_walk_diverges`]) probes those
//! chunks (and a handful of typed-parser blind spots in the raw XML) and, on any
//! hit, falls back to the OLD `parse_tree` walk for the WHOLE document — the
//! correctness escape hatch that keeps the corpus output byte-identical for
//! exotic markup. The `XmlNode`/`parse_tree` machinery is retained solely for
//! that fallback.

use std::{borrow::Cow, io, io::Cursor, sync::Arc};

use quick_xml::{
  Reader as XmlReader,
  events::{BytesStart, Event},
};
use rdocx_opc::OpcPackage;
use rdocx_oxml::document::{BodyContent, CT_Body, CT_Document};
use rdocx_oxml::drawing::CT_Drawing;
use rdocx_oxml::table::{CT_Tbl, CT_Tc, CellContent, VMerge};
use rdocx_oxml::text::{BreakType, CT_P, RunContent};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::cleaner::CleanedDocx;
use flowstate_document::{
  AssetId, AssetRecord, CellId, ColumnId, DocumentParagraphInput, InputBlock, InputBlockAlignment, InputImageBlock, InputImageSizing,
  InputParagraph, InputRun, InputTableBlock, InputTableCell, InputTableCellBlock, InputTableColumn, InputTableColumnWidth, InputTableRow,
  InputTableStyle, ParagraphStyle, RowId, RunStyles, SOFT_LINE_BREAK,
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
pub(super) fn interpret_structured(
  cleaned: &CleanedDocx,
  document: &CT_Document,
  paragraphs: &[DocumentParagraphInput],
) -> io::Result<StructuredDocx> {
  // The raw main-document XML is still needed: the parity pre-scan probes it for
  // typed-parser blind spots, and the fallback path parses it into `XmlNode`s.
  let owned_xml;
  let doc_xml: &[u8] = match cleaned.main_document_xml.as_deref() {
    Some(xml) => xml,
    None => match main_document_xml_from_package(cleaned) {
      Some(xml) => {
        owned_xml = xml;
        &owned_xml
      },
      None => return Ok(StructuredDocx::default()),
    },
  };

  // §A9.2 point 3: when the typed model provably cannot reproduce the old
  // XmlNode walk byte-for-byte (objects/text hiding in raw-captured wrappers, or
  // markup shapes the typed parser drops without capture), take the OLD path for
  // the whole document. Bounded parity risk, corpus-netted.
  // Escape hatch: FLOWSTATE_DOCX_TYPED_WALK=0 forces the old tree walk (parity
  // debugging + field fallback while the typed walk hardens).
  static TYPED_WALK_DISABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
  let typed_disabled = *TYPED_WALK_DISABLED.get_or_init(|| std::env::var_os("FLOWSTATE_DOCX_TYPED_WALK").is_some_and(|value| value == "0"));
  if typed_disabled || typed_walk_diverges(document, doc_xml, paragraphs.len()) {
    return interpret_structured_via_tree(cleaned, doc_xml, paragraphs);
  }

  let mut walker = TypedWalker {
    paragraphs,
    cursor: 0,
    blocks: Vec::new(),
    images: ImageAssets::new(cleaned),
    tables_imported: 0,
    images_imported: 0,
    equations_imported: 0,
  };
  walker.walk_body(&document.body);

  Ok(finish_structured(
    walker.blocks,
    walker.images.assets,
    walker.tables_imported,
    walker.images_imported,
    walker.equations_imported,
  ))
}

/// Fetches the uncompressed main-document XML when the cleaner did not capture
/// it (reuses the cleaner's in-memory package when available).
fn main_document_xml_from_package(cleaned: &CleanedDocx) -> Option<Vec<u8>> {
  let opened;
  let package: &OpcPackage = match cleaned.package.as_deref() {
    Some(package) => package,
    None => {
      opened = OpcPackage::from_reader(Cursor::new(cleaned.bytes.as_slice())).ok()?;
      &opened
    },
  };
  let main_part = package.main_document_part()?;
  package.get_part(&main_part).map(<[u8]>::to_vec)
}

/// Shared tail for both walks: guarantee the body flow ends in a paragraph row.
///
/// When the last imported block is an object (inline image/equation/table —
/// e.g. a doc whose final paragraph holds an image and is followed only by
/// `<w:sectPr>`), the projector otherwise FABRICATES a trailing paragraph with
/// no durable record (MissingParagraphMetadata/Block defects, boundary None).
/// Write that record ourselves: append the same Normal empty paragraph the
/// projector would synthesize, so the readback finds it.
fn finish_structured(
  mut blocks: Vec<InputBlock>,
  assets: Vec<(AssetId, AssetRecord)>,
  tables_imported: usize,
  images_imported: usize,
  equations_imported: usize,
) -> StructuredDocx {
  if !matches!(blocks.last(), Some(InputBlock::Paragraph(_))) {
    blocks.push(InputBlock::Paragraph(InputParagraph {
      style: ParagraphStyle::Normal,
      runs: Vec::new(),
    }));
  }
  StructuredDocx {
    blocks,
    assets,
    tables_imported,
    images_imported,
    equations_imported,
  }
}

// -- Image assets (shared by both walks) ------------------------------------

/// Image-asset resolution and dedup. The OPC package is the cleaner's
/// already-open in-memory one when available; otherwise it is opened LAZILY from the
/// packaged bytes on the first drawing that needs resolving, so tables-only
/// documents (the common carded-evidence case) never pay the unzip at all.
struct ImageAssets<'ctx> {
  cleaned: &'ctx CleanedDocx,
  /// `None` = not yet attempted; `Some(None)` = attempted and unavailable.
  #[allow(clippy::option_option, reason = "tri-state lazy-open memo: unattempted vs attempted-and-absent vs open")]
  opened: Option<Option<(Arc<OpcPackage>, String)>>,
  assets: Vec<(AssetId, AssetRecord)>,
  emitted_assets: FxHashSet<AssetId>,
}

impl<'ctx> ImageAssets<'ctx> {
  fn new(cleaned: &'ctx CleanedDocx) -> Self {
    Self {
      cleaned,
      opened: None,
      assets: Vec::new(),
      emitted_assets: FxHashSet::default(),
    }
  }

  /// A missing/unreadable package simply yields no image assets rather than
  /// failing the import (same behavior as the eager open it replaces).
  fn resolve_image(&mut self, relationship_id: &str) -> Option<AssetId> {
    let cleaned = self.cleaned;
    let opened = self
      .opened
      .get_or_insert_with(|| open_package_main_part(cleaned));
    let (package, main_part) = opened.as_ref()?;
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
          // B-S2: one header sniff at import — layout reads the stored value.
          dimensions: imagesize::blob_size(bytes)
            .ok()
            .map(|size| (size.width as u32, size.height as u32)),
          bytes: Arc::new(bytes.to_vec()),
          render_image: Arc::default(),
        },
      ));
    }
    Some(asset_id)
  }

  /// §act-eleven A11.9: resolve a relationship id to its EXTERNAL target URL
  /// (`TargetMode="External"`). A genuinely-LINKED image has no part bytes, so
  /// no [`AssetRecord`] is created; the returned [`AssetId`] is derived from
  /// the URL bytes (same hash as [`asset_id_from_bytes`]) so the id stays
  /// stable across imports. Internal-mode (or missing) relationships return
  /// `None` — the embedded path is never affected.
  fn resolve_external_image(&mut self, relationship_id: &str) -> Option<(AssetId, String)> {
    let cleaned = self.cleaned;
    let opened = self
      .opened
      .get_or_insert_with(|| open_package_main_part(cleaned));
    let (package, main_part) = opened.as_ref()?;
    let relationship = package
      .get_part_rels(main_part)?
      .get_by_id(relationship_id)?;
    if !relationship
      .target_mode
      .as_deref()
      .is_some_and(|mode| mode.eq_ignore_ascii_case("external"))
    {
      return None;
    }
    // rdocx-opc keeps attribute values RAW (no XML unescape); URLs routinely
    // carry `&amp;` in query strings — decode to the real URL.
    let url = unescape_attribute_value(&relationship.target);
    if url.is_empty() {
      return None;
    }
    Some((asset_id_from_bytes(url.as_bytes()), url))
  }

  /// §act-ten A9.6: legacy VML images (`<w:pict><v:imagedata r:id=…/>`), the
  /// pre-DrawingML form Word still emits for pasted/legacy content. Both walks
  /// dropped these wholesale (one of the four corpus roundtrip residuals is a
  /// 27-image VML doc). Extract every `v:imagedata` relationship, resolve it
  /// through the SAME asset path as `DrawingML` blips, and size from the
  /// enclosing shape's `style="width:…pt;height:…pt"` when present.
  /// §act-eleven A11.9: when no embeddable part resolves via `r:id`/`o:relid`
  /// but `r:href` names an external-mode relationship, the image imports as a
  /// LINKED image carrying that URL instead of being dropped.
  fn vml_images_from_chunk(&mut self, chunk: &[u8]) -> Vec<InputImageBlock> {
    if memchr::memmem::find(chunk, b"<v:imagedata").is_none() && memchr::memmem::find(chunk, b"<v:imageData").is_none() {
      return Vec::new();
    }
    let mut images = Vec::new();
    let mut reader = quick_xml::Reader::from_reader(chunk);
    reader.config_mut().check_end_names = false;
    let mut buf = Vec::new();
    let mut shape_extent_px: Option<(u32, Option<u32>)> = None;
    loop {
      match reader.read_event_into(&mut buf) {
        Ok(quick_xml::events::Event::Start(node)) | Ok(quick_xml::events::Event::Empty(node)) => {
          let name = node.name();
          let local = local_name(name.as_ref());
          if matches!(local, "shape" | "rect" | "oval") {
            shape_extent_px = vml_style_extent_px(&node);
          } else if local == "imagedata" {
            let mut rel_id = None;
            let mut href_id = None;
            for attribute in node.attributes().flatten() {
              let key = local_name(attribute.key.as_ref());
              // `r:id` is the embedded part; `o:relid` is the legacy Office
              // alias. `r:href` is the EXTERNAL companion (no part bytes) —
              // used only when no embeddable part resolves (§A11.9).
              if matches!(key, "id" | "relid") {
                rel_id = Some(String::from_utf8_lossy(&attribute.value).into_owned());
              } else if key == "href" {
                href_id = Some(String::from_utf8_lossy(&attribute.value).into_owned());
              }
            }
            let sizing = match shape_extent_px {
              Some((width_px, height_px)) => InputImageSizing::Fixed { width_px, height_px },
              None => InputImageSizing::Intrinsic,
            };
            if let Some(asset_id) = rel_id.and_then(|rel_id| self.resolve_image(&rel_id)) {
              images.push(InputImageBlock {
                asset_id,
                alt_text: String::new(),
                sizing,
                alignment: InputBlockAlignment::Left,
                external_url: None,
              });
            } else if let Some((asset_id, url)) = href_id.and_then(|href_id| self.resolve_external_image(&href_id)) {
              // §A11.9: no embeddable part, but the `r:href` companion is a
              // genuine external-mode relationship — import as a LINKED image.
              images.push(InputImageBlock {
                asset_id,
                alt_text: String::new(),
                sizing,
                alignment: InputBlockAlignment::Left,
                external_url: Some(url),
              });
            }
          }
        },
        Ok(quick_xml::events::Event::Eof) | Err(_) => break,
        _ => {},
      }
      buf.clear();
    }
    images
  }
}

/// Parse a VML shape `style` attribute's `width`/`height` point values into
/// pixels (96 dpi, the DrawingML-import convention).
fn vml_style_extent_px(node: &quick_xml::events::BytesStart<'_>) -> Option<(u32, Option<u32>)> {
  let style = node
    .attributes()
    .flatten()
    .find(|attribute| local_name(attribute.key.as_ref()) == "style")?;
  let style = String::from_utf8_lossy(&style.value).into_owned();
  let dimension_px = |key: &str| -> Option<u32> {
    let value = style.split(';').find_map(|entry| {
      entry
        .trim()
        .strip_prefix(key)
        .and_then(|rest| rest.trim_start().strip_prefix(':'))
    })?;
    let points: f64 = value.trim().trim_end_matches("pt").parse().ok()?;
    let px = (points * 96.0 / 72.0).round();
    (px >= 1.0).then_some(px as u32)
  };
  let width = dimension_px("width")?;
  Some((width, dimension_px("height")))
}

fn open_package_main_part(cleaned: &CleanedDocx) -> Option<(Arc<OpcPackage>, String)> {
  // §act-nine A9.2: reuse the package the cleaner already opened (unzip #1)
  // instead of re-inflating from bytes; the from-bytes open remains only as a
  // fallback for callers that built a `CleanedDocx` without one.
  let package = match cleaned.package.clone() {
    Some(package) => package,
    None => Arc::new(OpcPackage::from_reader(Cursor::new(cleaned.bytes.as_slice())).ok()?),
  };
  let main_part = package.main_document_part()?;
  Some((package, main_part))
}

// -- Typed walk (the fast path) ----------------------------------------------

struct TypedWalker<'ctx> {
  paragraphs: &'ctx [DocumentParagraphInput],
  cursor: usize,
  blocks: Vec<InputBlock>,
  images: ImageAssets<'ctx>,
  tables_imported: usize,
  images_imported: usize,
  equations_imported: usize,
}

impl TypedWalker<'_> {
  fn walk_body(&mut self, body: &CT_Body) {
    for content in &body.content {
      match content {
        BodyContent::Paragraph(paragraph) => self.walk_paragraph(paragraph),
        BodyContent::Table(table) => {
          let table = self.parse_table(table);
          self.blocks.push(InputBlock::Table(table));
        },
        BodyContent::RawXml(chunk) => {
          // Parity note (§A9.2 point 3): a SELF-CLOSED `<w:p/>` parses as
          // `RawXml` (the typed body parser only types `Start`-form paragraphs),
          // but the old XmlNode walker treated it as a paragraph and consumed a
          // recognized-slice entry — replicate that exactly so the cursor and
          // the emitted block sequence stay identical. All other raw chunks were
          // vetted object-free by `typed_walk_diverges` and are skipped, which
          // matches the old walker (it only descended into `p`/`tbl` children).
          if chunk_root_local(chunk) == b"p" {
            self.walk_empty_paragraph();
          }
        },
      }
    }
  }

  /// The typed-parser image of a self-closed `<w:p/>`: no runs, no objects.
  fn walk_empty_paragraph(&mut self) {
    let paragraph = if self.cursor < self.paragraphs.len() {
      let input = input_paragraph_from_document(&self.paragraphs[self.cursor]);
      self.cursor += 1;
      input
    } else {
      InputParagraph {
        style: ParagraphStyle::Normal,
        runs: Vec::new(),
      }
    };
    self.blocks.push(InputBlock::Paragraph(paragraph));
  }

  fn walk_paragraph(&mut self, paragraph_node: &CT_P) {
    // Top-level paragraphs are 1:1 with the pre-computed slice; the cursor only
    // advances on `w:p`, so tables, section properties and SDTs never desync it.
    let paragraph = if self.cursor < self.paragraphs.len() {
      let input = input_paragraph_from_document(&self.paragraphs[self.cursor]);
      self.cursor += 1;
      input
    } else {
      cell_paragraph_typed(paragraph_node)
    };

    // Resolve the paragraph's inline objects (images AND equations) first so we
    // can tell whether this <w:p> is a bare OBJECT wrapper. Drawings come from
    // the typed run content; equations come from the `m:oMath`/`m:oMathPara`
    // chunks the typed parser captured verbatim into `extra_xml` (exactly the
    // `&[u8]` input `omml::equations_from_container_bytes` takes — no byte
    // spans needed). Objects hiding in OTHER wrappers were routed to the
    // fallback by `typed_walk_diverges`.
    let mut images = Vec::new();
    for run in &paragraph_node.runs {
      for content in &run.content {
        if let RunContent::Drawing(drawing) = content
          && let Some(image) = self.image_from_drawing(drawing)
        {
          images.push(image);
        }
      }
      // §act-ten A9.6: legacy VML picts land in the runs' raw chunks (unknown
      // to the typed parser). The parity pre-scan lets these through (they
      // carry no `drawing`/`oMath`/`tbl` markers), and the OLD walker collects
      // them identically, so both paths import VML the same way.
      for chunk in &run.extra_xml {
        images.extend(self.images.vml_images_from_chunk(chunk));
      }
    }
    for (_, chunk) in &paragraph_node.extra_xml {
      images.extend(self.images.vml_images_from_chunk(chunk));
    }
    let equations: Vec<_> = paragraph_node
      .extra_xml
      .iter()
      .filter(|(_, chunk)| omml::contains_office_math(chunk))
      .flat_map(|(_, chunk)| omml::equations_from_container_bytes(chunk))
      .collect();

    // §perf-heaven T7.18/T7.21/T8.10 (see the fallback walker for the full
    // rationale): a <w:p> that carries object(s) and no paragraph text is a bare
    // object wrapper — emit only the inline object(s), no empty Paragraph.
    let paragraph_is_text_empty = paragraph.runs.iter().all(|run| run.text.trim().is_empty());
    let has_objects = !images.is_empty() || !equations.is_empty();
    let collapse_bare_object_wrapper = has_objects && paragraph_is_text_empty;
    if !collapse_bare_object_wrapper {
      self.blocks.push(InputBlock::Paragraph(paragraph));
    }

    for image in images {
      self.blocks.push(InputBlock::Image(image));
      self.images_imported += 1;
    }

    for equation in equations {
      self.blocks.push(InputBlock::Equation(equation));
      self.equations_imported += 1;
    }
  }

  fn parse_table(&mut self, table_node: &CT_Tbl) -> InputTableBlock {
    self.tables_imported += 1;
    let column_widths: Vec<InputTableColumnWidth> = table_node
      .grid
      .as_ref()
      .map(|grid| {
        grid
          .columns
          .iter()
          .map(|column| column_width_from_twips(i64::from(column.width.0)))
          .collect()
      })
      .unwrap_or_default();
    let mut rows: Vec<InputTableRow> = Vec::new();
    let mut header_row = false;
    let mut first_row = true;
    // Widest row in grid columns, so the durable columns cover every cell's
    // grid position even when a row spans more columns than `tblGrid` declares.
    let mut grid_width = 0_usize;
    // grid column -> (row index, cell index) of the cell that started a vertical
    // merge, so continuation cells fold into its `row_span` instead of emitting.
    let mut vertical_open: FxHashMap<usize, (usize, usize)> = FxHashMap::default();

    for row_node in &table_node.rows {
      if first_row {
        first_row = false;
        // `<w:tblHeader/>` only — attributed/expanded forms fall back to the
        // old walk (the typed parser records bare presence, see the pre-scan).
        header_row = row_node
          .properties
          .as_ref()
          .is_some_and(|properties| properties.header == Some(true));
      }
      // Deterministic row id, seeded per-table so row ids are GLOBALLY unique
      // across every table in the document (see the fallback walker for the full
      // rationale). The increment ORDER matches the old walk exactly — nested
      // tables bump the seed lazily, when their anchor cell's blocks are built.
      let row_id = RowId(((self.tables_imported as u128) << 40) | (rows.len() as u128 + 1));
      let mut cells: Vec<InputTableCell> = Vec::new();
      let mut grid_col = 0_usize;
      for cell_node in &row_node.cells {
        grid_col = self.add_table_cell(cell_node, row_id, grid_col, &mut rows, &mut cells, &mut vertical_open);
      }
      grid_width = grid_width.max(grid_col);
      rows.push(InputTableRow { id: row_id, cells });
    }

    let columns = build_table_columns(&column_widths, grid_width);
    fill_full_grid(&mut rows, &columns);
    InputTableBlock {
      rows,
      columns,
      style: InputTableStyle { header_row },
    }
  }

  /// Appends one `w:tc` to the current row (or folds a vertical-merge
  /// continuation into the originating cell above) and returns the next grid
  /// column. `rows` holds the already-completed rows; `cells` is the row in
  /// progress.
  fn add_table_cell(
    &mut self,
    cell_node: &CT_Tc,
    row_id: RowId,
    grid_col: usize,
    rows: &mut [InputTableRow],
    cells: &mut Vec<InputTableCell>,
    vertical_open: &mut FxHashMap<usize, (usize, usize)>,
  ) -> usize {
    let col_span = cell_grid_span_typed(cell_node);
    // The cell's durable column is the sequential id of its starting grid column.
    let column_id = ColumnId(grid_col as u128 + 1);
    match cell_vertical_merge_typed(cell_node) {
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
          id: CellId::from_coordinate(row_id, column_id),
          row_id,
          column_id,
          blocks,
          row_span: 1,
          col_span,
        });
      },
      VerticalMerge::None => {
        vertical_open.remove(&grid_col);
        let blocks = self.cell_blocks(cell_node);
        cells.push(InputTableCell {
          id: CellId::from_coordinate(row_id, column_id),
          row_id,
          column_id,
          blocks,
          row_span: 1,
          col_span,
        });
      },
    }
    grid_col + usize::from(col_span.max(1))
  }

  fn cell_blocks(&mut self, cell_node: &CT_Tc) -> Vec<InputTableCellBlock> {
    let mut blocks = Vec::new();
    for content in &cell_node.content {
      match content {
        CellContent::Paragraph(paragraph) => blocks.push(InputTableCellBlock::Paragraph(cell_paragraph_typed(paragraph))),
        CellContent::Table(nested) => blocks.push(InputTableCellBlock::Table(self.parse_table(nested))),
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

  fn image_from_drawing(&mut self, drawing: &CT_Drawing) -> Option<InputImageBlock> {
    let (embed_id, extent_cx, extent_cy, description, name) = match (drawing.inline.as_ref(), drawing.anchor.as_ref()) {
      (Some(inline), _) => (
        inline.embed_id.as_str(),
        inline.extent_cx.0,
        inline.extent_cy.0,
        inline.description.as_deref(),
        inline.name.as_deref(),
      ),
      (None, Some(anchor)) => (
        anchor.embed_id.as_str(),
        anchor.extent_cx.0,
        anchor.extent_cy.0,
        anchor.description.as_deref(),
        anchor.name.as_deref(),
      ),
      (None, None) => return None,
    };
    // §perf-heaven T7.20 parity: rdocx keeps attribute values RAW (quick-xml
    // does not auto-unescape attributes) while the old XmlNode walker unescaped
    // them — Word writes multi-line alt text with `&#xA;` entities.
    let alt_text = unescape_attribute_value(description.or(name).unwrap_or_default());
    // No `a:blip r:embed` found by the typed parse. §act-eleven A11.9: a blip
    // carrying `r:link` INSTEAD (a genuinely-linked image) resolves through the
    // external-mode relationship to its URL; the typed model does not parse
    // `r:link`, so it is read from the drawing's preserved raw bytes. Anything
    // else (VML-only drawings, malformed blips) is skipped as before.
    if embed_id.is_empty() {
      let link_id = blip_link_id_from_raw(drawing_raw_xml(drawing))?;
      let (asset_id, url) = self.images.resolve_external_image(&link_id)?;
      return Some(InputImageBlock {
        asset_id,
        alt_text,
        sizing: sizing_from_extent_emu(extent_cx, extent_cy),
        alignment: InputBlockAlignment::Left,
        external_url: Some(url),
      });
    }
    let asset_id = self.images.resolve_image(embed_id)?;
    Some(InputImageBlock {
      asset_id,
      alt_text,
      sizing: sizing_from_extent_emu(extent_cx, extent_cy),
      alignment: InputBlockAlignment::Left,
      external_url: None,
    })
  }
}

/// Lightweight cell-paragraph recognition over the typed model: table-cell
/// paragraphs are not part of the recognized slice, so they are imported as
/// Normal-styled plain text. This trades run-level semantics inside tables for
/// a small, robust path. The text mapping mirrors [`collect_run_text`]:
/// * U+FFFC object-placeholder sentinels are stripped (reserved body-flow
///   encoding, see `soften_rdocx_breaks`);
/// * `w:tab` → '\t';
/// * a LINE `w:br` → the model's soft break (U+2028), NEVER '\n' ('\n' is the
///   paragraph separator in the body flow — §perf-heaven T7.22);
/// * page/column breaks are pagination, dropped (no inline representation);
/// * drawings/fields carrying harvestable text and `<w:cr>` never reach here —
///   the parity pre-scan routes those documents to the fallback walk.
fn cell_paragraph_typed(paragraph_node: &CT_P) -> InputParagraph {
  let mut text = String::new();
  for run in &paragraph_node.runs {
    for content in &run.content {
      match content {
        RunContent::Text(run_text) => text.extend(run_text.text.chars().filter(|&ch| ch != '\u{FFFC}')),
        RunContent::Tab => text.push('\t'),
        RunContent::Break(BreakType::Line) => text.push(SOFT_LINE_BREAK),
        RunContent::Break(BreakType::Page | BreakType::Column) => {},
        RunContent::Drawing(_) | RunContent::Field { .. } | RunContent::FootnoteRef { .. } | RunContent::EndnoteRef { .. } => {},
      }
    }
  }
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

fn cell_grid_span_typed(cell_node: &CT_Tc) -> u16 {
  cell_node
    .properties
    .as_ref()
    .and_then(|properties| properties.grid_span)
    .and_then(|span| u16::try_from(span).ok())
    .filter(|span| *span >= 1)
    .unwrap_or(1)
}

fn cell_vertical_merge_typed(cell_node: &CT_Tc) -> VerticalMerge {
  // `<w:vMerge/>` and `w:val="continue"` continue the merge above; only an
  // explicit `restart` opens a new vertical span (the typed parser already
  // folds every non-`restart` value to `Continue`).
  match cell_node
    .properties
    .as_ref()
    .and_then(|properties| properties.v_merge)
  {
    None => VerticalMerge::None,
    Some(VMerge::Restart) => VerticalMerge::Restart,
    Some(VMerge::Continue) => VerticalMerge::Continue,
  }
}

// -- Parity pre-scan (§A9.2 point 3) -----------------------------------------

/// Substrings whose presence in a raw-captured chunk means an object the typed
/// walk cannot see (same bare-substring reasoning as the `object_free` gate in
/// `build_structured_document`: a real object's start tag always contains its
/// local name; false positives only forgo the fast path).
const OBJECT_PROBES: &[&[u8]] = &[b"tbl", b"drawing", b"oMath"];

/// Substrings that mean HARVESTABLE CELL TEXT hides inside a raw-captured
/// wrapper (or a drawing) — the old walk's recursive `collect_run_text`
/// descended into everything and collected every `t`-local text (`w:t`, `m:t`,
/// `DrawingML` `a:t`), tabs and line breaks. Prefix matches over-trigger
/// (`<w:t` also hits `<w:tab`, which the old walk ALSO collected) — safe,
/// conservative direction.
const CELL_TEXT_PROBES: &[&[u8]] = &[b"<w:t", b"<m:t", b"<a:t", b"<w:br"];

/// Wrappers (and math) the typed paragraph parser silently DROPS when they sit
/// directly inside `w:hyperlink` (its inner loop keeps only `w:r` children,
/// with no raw capture) — the old walk descended into them.
/// (`<w:ins `/`<w:del ` keep their trailing space: tracked-change wrappers
/// always carry attributes, while the bare prefixes would false-positive on
/// `<w:instrText`/`<w:delText`, which live INSIDE runs and are typed-handled.)
const HYPERLINK_INTERIOR_PROBES: &[&[u8]] = &[
  b"oMath",
  b"<w:sdt",
  b"<w:smartTag",
  b"<w:ins ",
  b"<w:del ",
  b"<w:fldSimple",
  b"<mc:AlternateContent",
];

/// Whether the typed walk can reproduce the old `XmlNode` walk byte-for-byte.
/// Every trigger is a place where the typed model either captures content as
/// raw bytes (unknown wrappers) or drops it without capture (typed-parser
/// blind spots probed on the raw XML) while the old walk consumed it.
#[hotpath::measure]
fn typed_walk_diverges(document: &CT_Document, doc_xml: &[u8], recognized_paragraphs: usize) -> bool {
  let mut has_table = false;
  let mut paragraph_ix = 0_usize;
  for content in &document.body.content {
    match content {
      BodyContent::RawXml(chunk) => {
        // Objects inside an unknown BODY-LEVEL wrapper (top-level w:sdt,
        // mc:AlternateContent, ...) — fall back so the exotic wrapper keeps its
        // old-walk treatment.
        if contains_any(chunk, OBJECT_PROBES) {
          return true;
        }
        // A self-closed `<w:p/>` consumes a slice entry (see `walk_body`).
        if chunk_root_local(chunk) == b"p" {
          paragraph_ix += 1;
        }
      },
      BodyContent::Paragraph(paragraph) => {
        // Beyond-slice paragraphs take the cell-paragraph path in the walk, so
        // they inherit the cell-text parity triggers too.
        let treat_as_cell = paragraph_ix >= recognized_paragraphs;
        paragraph_ix += 1;
        if body_paragraph_diverges(paragraph, treat_as_cell) {
          return true;
        }
      },
      BodyContent::Table(table) => {
        has_table = true;
        if table_diverges(table) {
          return true;
        }
      },
    }
  }
  raw_probes_diverge(doc_xml, has_table)
}

fn body_paragraph_diverges(paragraph: &CT_P, treat_as_cell: bool) -> bool {
  if treat_as_cell && cell_paragraph_diverges(paragraph) {
    return true;
  }
  for (_, chunk) in &paragraph.extra_xml {
    // A drawing inside ANY paragraph-level raw chunk (unknown wrapper, or even
    // nested inside a captured oMath) was found by the old `collect_descendants`.
    if contains_any(chunk, &[b"drawing"]) {
      return true;
    }
    // `m:oMath`/`m:oMathPara`-ROOTED chunks are the typed walk's equation input
    // (fed verbatim to omml); oMath inside any OTHER wrapper is invisible to it.
    if !chunk_root_is_office_math(chunk) && contains_any(chunk, &[b"oMath"]) {
      return true;
    }
  }
  for run in &paragraph.runs {
    for chunk in &run.extra_xml {
      // Run-level unknown wrappers (mc:AlternateContent, w:pict, w:object, ...)
      // hiding a drawing or math.
      if contains_any(chunk, OBJECT_PROBES) {
        return true;
      }
    }
    for content in &run.content {
      if let RunContent::Drawing(drawing) = content {
        let raw = drawing_raw_xml(drawing);
        // Equations inside a drawing's text box were reachable through the old
        // whole-paragraph byte span.
        if contains_any(raw, &[b"oMath"]) {
          return true;
        }
        // Multiple embedded blips in one drawing: the old walk took the FIRST
        // `blip@r:embed` in document order, the typed parse keeps the LAST.
        if memchr::memmem::find_iter(raw, b":embed=").take(2).count() > 1 {
          return true;
        }
      }
    }
  }
  false
}

fn cell_paragraph_diverges(paragraph: &CT_P) -> bool {
  // pPr>tabs>w:tab STOP DEFINITIONS: the old recursive text collection matched
  // every element with local name `tab` — including tab-stop definitions inside
  // pPr — fabricating one '\t' per stop in the cell text. Bug-for-bug parity is
  // owed to the corpus net, so route those documents to the old walk.
  if paragraph
    .properties
    .as_ref()
    .is_some_and(|properties| properties.tabs.is_some())
  {
    return true;
  }
  for (_, chunk) in &paragraph.extra_xml {
    if contains_any(chunk, CELL_TEXT_PROBES) {
      return true;
    }
  }
  for run in &paragraph.runs {
    for chunk in &run.extra_xml {
      if contains_any(chunk, CELL_TEXT_PROBES) {
        return true;
      }
    }
    for content in &run.content {
      match content {
        // `w:fldSimple` becomes a synthetic Field run and its display runs are
        // skipped by the typed parse; the old walk collected their text.
        RunContent::Field { .. } => return true,
        // Text boxes inside a cell drawing: the old walk collected their
        // `w:t`/`a:t` descendants into the cell text.
        RunContent::Drawing(drawing) if contains_any(drawing_raw_xml(drawing), CELL_TEXT_PROBES) => {
          return true;
        },
        _ => {},
      }
    }
  }
  false
}

fn table_diverges(table: &CT_Tbl) -> bool {
  table.rows.iter().any(|row| {
    row.cells.iter().any(|cell| {
      cell.content.iter().any(|content| match content {
        CellContent::Paragraph(paragraph) => cell_paragraph_diverges(paragraph),
        CellContent::Table(nested) => table_diverges(nested),
      })
    })
  })
}

/// Raw-XML probes for shapes the typed parser drops WITHOUT capturing anything
/// (so no typed-side trigger can exist for them).
fn raw_probes_diverge(doc_xml: &[u8], has_table: bool) -> bool {
  if has_table {
    // `<w:cr/>` is dropped by the typed run parse; the old cell walk emitted a
    // soft break for it. Body paragraphs are unaffected (their text comes from
    // the same rdocx parse on both paths), so this only matters with tables.
    if contains_any(doc_xml, &[b"<w:cr"]) {
      return true;
    }
    // Table-control blind spots: the typed parser reads these from EMPTY
    // (self-closing) events only, and records `tblHeader` as bare presence
    // (ignoring `w:val`). Expanded (`<w:vMerge>...</w:vMerge>`) or attributed
    // `tblHeader` forms therefore diverge from the old attr-aware walk.
    if tbl_header_probe_diverges(doc_xml) {
      return true;
    }
    for tag in [&b"<w:vMerge"[..], b"<w:gridSpan", b"<w:gridCol"] {
      if tag_occurrence_matches(doc_xml, tag, false) {
        return true;
      }
    }
    // Self-closed `<w:tr/>`/`<w:tc/>` are dropped by the typed table parse but
    // produced (empty) rows/cells in the old walk.
    if tag_occurrence_matches(doc_xml, b"<w:tr", true) || tag_occurrence_matches(doc_xml, b"<w:tc", true) {
      return true;
    }
    // Self-closed `<w:p/>` INSIDE a table cell: `CT_Tc` drops it silently, but
    // the old walk emitted an empty cell paragraph for it. A cell whose ONLY
    // paragraph is one self-closed `<w:p/>` is benign — the typed walk's empty
    // placeholder equals the old empty paragraph — so only a dropped `w:p`
    // COEXISTING with other cell paragraphs diverges (this keeps the fast path
    // for the very common Word empty-cell shape). (Body-level self-closed
    // paragraphs are typed as RawXml and replayed exactly — see `walk_body`.)
    // A nested table truncates its host cell's span at the first inner
    // `</w:tc>`; the host's trailing region is the one documented residual this
    // span heuristic can miss.
    if any_wrapped_span(doc_xml, b"<w:tc", b"</w:tc>", cell_span_drops_paragraph) {
      return true;
    }
  }
  // The typed paragraph parser keeps only `w:r` children of `w:hyperlink` and
  // skips everything else with NO raw capture; the old walk descended into the
  // whole hyperlink subtree (equations there were imported; wrapped text inside
  // cells was collected).
  if any_wrapped_span(doc_xml, b"<w:hyperlink", b"</w:hyperlink>", |span| {
    contains_any(span, HYPERLINK_INTERIOR_PROBES)
  }) {
    return true;
  }
  // `w:fldSimple` children are skipped by the typed parse; the old walk found
  // drawings/equations inside them.
  any_wrapped_span(doc_xml, b"<w:fldSimple", b"</w:fldSimple>", |span| {
    contains_any(span, &[b"drawing", b"oMath"])
  })
}

/// `<w:tblHeader/>` is the only form both parsers agree on; anything else
/// (attributes — the typed parser ignores `w:val="false"` — or an expanded
/// element it cannot see at all) falls back.
fn tbl_header_probe_diverges(doc_xml: &[u8]) -> bool {
  memchr::memmem::find_iter(doc_xml, b"<w:tblHeader").any(|start| doc_xml.get(start + b"<w:tblHeader".len()) != Some(&b'/'))
}

/// Whether a table-cell span holds a self-closed `<w:p/>` ALONGSIDE at least
/// one other paragraph (the shape where the typed parse's silent drop changes
/// the emitted block list — see the call site). Truncated markup conservatively
/// diverges.
fn cell_span_drops_paragraph(span: &[u8]) -> bool {
  let mut paragraphs = 0_usize;
  let mut self_closed = 0_usize;
  for start in memchr::memmem::find_iter(span, b"<w:p") {
    let rest = &span[start + b"<w:p".len()..];
    // Name boundary: `<w:p` must not count `<w:pPr`, `<w:pStyle`, `<w:pict`, ...
    match rest.first() {
      None => return true,
      Some(b' ' | b'\t' | b'\r' | b'\n' | b'/' | b'>') => {},
      Some(_) => continue,
    }
    paragraphs += 1;
    let Some(close) = memchr::memchr(b'>', rest) else {
      return true;
    };
    if close > 0 && rest[close - 1] == b'/' {
      self_closed += 1;
    }
  }
  self_closed >= 1 && paragraphs >= 2
}

/// Whether any occurrence of `tag_open` (name-boundary checked) is self-closed
/// (`self_closed == true`) or expanded (`self_closed == false`). A truncated
/// tag (no `>` found) conservatively matches either form.
fn tag_occurrence_matches(haystack: &[u8], tag_open: &[u8], self_closed: bool) -> bool {
  for start in memchr::memmem::find_iter(haystack, tag_open) {
    let rest = &haystack[start + tag_open.len()..];
    // The needle must end the tag NAME (`<w:p` must not match `<w:pPr`).
    match rest.first() {
      None => return true,
      Some(b' ' | b'\t' | b'\r' | b'\n' | b'/' | b'>') => {},
      Some(_) => continue,
    }
    let Some(close) = memchr::memchr(b'>', rest) else {
      return true;
    };
    let occurrence_self_closed = close > 0 && rest[close - 1] == b'/';
    if occurrence_self_closed == self_closed {
      return true;
    }
  }
  false
}

/// Runs `probe` over every `open`..`close` span. Non-nesting elements only; a
/// self-closed `open` with a later sibling makes the span overshoot into
/// following content, which can only OVER-trigger the fallback (safe direction).
fn any_wrapped_span(doc_xml: &[u8], open: &[u8], close: &[u8], probe: impl Fn(&[u8]) -> bool) -> bool {
  for start in memchr::memmem::find_iter(doc_xml, open) {
    let rest = &doc_xml[start + open.len()..];
    let Some(end) = memchr::memmem::find(rest, close) else {
      continue;
    };
    if probe(&rest[..end]) {
      return true;
    }
  }
  false
}

fn contains_any(haystack: &[u8], needles: &[&[u8]]) -> bool {
  needles
    .iter()
    .any(|needle| memchr::memmem::find(haystack, needle).is_some())
}

/// Local name of a raw-captured chunk's root element (empty when malformed).
fn chunk_root_local(chunk: &[u8]) -> &[u8] {
  let Some(rest) = chunk.strip_prefix(b"<") else {
    return &[];
  };
  let name_end = rest
    .iter()
    .position(|&byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n' | b'/' | b'>'))
    .unwrap_or(rest.len());
  let name = &rest[..name_end];
  name.rsplit(|&byte| byte == b':').next().unwrap_or(name)
}

fn chunk_root_is_office_math(chunk: &[u8]) -> bool {
  let local = chunk_root_local(chunk);
  local == b"oMath" || local == b"oMathPara"
}

/// The verbatim bytes of a drawing's `wp:inline`/`wp:anchor` (rdocx preserves
/// them for round-trip). Empty when absent — a hand-constructed drawing, which
/// never comes out of the parser this walk consumes.
fn drawing_raw_xml(drawing: &CT_Drawing) -> &[u8] {
  drawing
    .inline
    .as_ref()
    .and_then(|inline| inline.raw_xml.as_deref())
    .or_else(|| {
      drawing
        .anchor
        .as_ref()
        .and_then(|anchor| anchor.raw_xml.as_deref())
    })
    .unwrap_or_default()
}

/// §act-eleven A11.9: the `a:blip r:link` relationship id in a drawing's raw
/// `wp:inline`/`wp:anchor` bytes, for blips that carry NO `r:embed`. The typed
/// model (`CT_Inline`/`CT_Anchor`) only parses `r:embed`, so link-only blips
/// are recovered from the verbatim bytes rdocx preserves. A blip that carries
/// `r:embed` returns `None` (it is an embedded image, handled by the typed
/// field; `r:link` on such a blip is only Word's edit-time refresh hint).
fn blip_link_id_from_raw(raw: &[u8]) -> Option<String> {
  memchr::memmem::find(raw, b":link=")?;
  let mut reader = XmlReader::from_reader(raw);
  reader.config_mut().check_end_names = false;
  let mut buf = Vec::new();
  loop {
    match reader.read_event_into(&mut buf) {
      Ok(Event::Start(node)) | Ok(Event::Empty(node)) => {
        if local_name(node.name().as_ref()) == "blip" {
          let mut link = None;
          for attribute in node.attributes().flatten() {
            match local_name(attribute.key.as_ref()) {
              "embed" => return None,
              "link" => link = Some(String::from_utf8_lossy(&attribute.value).into_owned()),
              _ => {},
            }
          }
          return link.filter(|id| !id.is_empty());
        }
      },
      Ok(Event::Eof) | Err(_) => return None,
      _ => {},
    }
    buf.clear();
  }
}

// -- Fallback walk over the owned XmlNode tree (§A9.2 escape hatch) ----------

/// The pre-A9.2 walk, retained verbatim as the whole-document fallback for
/// markup the typed model cannot reproduce byte-for-byte.
#[hotpath::measure]
fn interpret_structured_via_tree(cleaned: &CleanedDocx, doc_xml: &[u8], paragraphs: &[DocumentParagraphInput]) -> io::Result<StructuredDocx> {
  let Some(root) = parse_tree(doc_xml) else {
    return Ok(StructuredDocx::default());
  };
  let Some(body) = child(&root, "body") else {
    return Ok(StructuredDocx::default());
  };

  let mut walker = StructuredWalker {
    doc_xml,
    paragraphs,
    cursor: 0,
    blocks: Vec::new(),
    images: ImageAssets::new(cleaned),
    tables_imported: 0,
    images_imported: 0,
    equations_imported: 0,
  };
  walker.walk_body(body);

  Ok(finish_structured(
    walker.blocks,
    walker.images.assets,
    walker.tables_imported,
    walker.images_imported,
    walker.equations_imported,
  ))
}

struct StructuredWalker<'ctx> {
  doc_xml: &'ctx [u8],
  paragraphs: &'ctx [DocumentParagraphInput],
  cursor: usize,
  blocks: Vec<InputBlock>,
  images: ImageAssets<'ctx>,
  tables_imported: usize,
  images_imported: usize,
  equations_imported: usize,
}

impl StructuredWalker<'_> {
  fn walk_body(&mut self, body: &XmlNode) {
    for node in &body.children {
      match node.local.as_ref() {
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

    // Resolve the paragraph's inline objects (images AND equations) first so we
    // can tell whether this <w:p> is a bare OBJECT wrapper.
    let mut drawings = Vec::new();
    collect_descendants(paragraph_node, "drawing", &mut drawings);
    let mut images: Vec<_> = drawings
      .into_iter()
      .filter_map(|drawing| self.image_from_drawing(drawing))
      .collect();
    // §act-ten A9.6: legacy VML picts (same collection as the typed walk, off
    // the pict subtree's raw byte span so both paths import VML identically).
    let mut picts = Vec::new();
    collect_descendants(paragraph_node, "pict", &mut picts);
    for pict in picts {
      if let Some(span) = self.doc_xml.get(pict.start..pict.end) {
        images.extend(self.images.vml_images_from_chunk(span));
      }
    }
    let equations: Vec<_> = self
      .doc_xml
      .get(paragraph_node.start..paragraph_node.end)
      .filter(|bytes| omml::contains_office_math(bytes))
      .map(omml::equations_from_container_bytes)
      .unwrap_or_default();

    // §perf-heaven T7.18/T7.21: an image OR an equation is an INLINE object (a
    // U+FFFC anchored in the surrounding paragraph's flow), NOT a block-level
    // paragraph — both go through `push_object` in `loro_import`. But the exporter
    // writes each as its OWN <w:p> (image = a drawing run; equation = an
    // `m:oMath` paragraph), so a naive reimport pushes an EMPTY Paragraph whose
    // `\n` boundary is a spurious extra separator around the object. When a <w:p>
    // carries object(s) and no paragraph text, collapse it: emit only the inline
    // object(s), no empty Paragraph. A paragraph with BOTH text and an object
    // keeps its Paragraph block (the object stays inline within it).
    let paragraph_is_text_empty = paragraph.runs.iter().all(|run| run.text.trim().is_empty());
    let has_objects = !images.is_empty() || !equations.is_empty();
    // §perf-heaven T8.10: collapse a bare object wrapper even at the START of the
    // document. The import plan always begins with a sentinel `\n` (a Normal first
    // paragraph, `FlowTextImportPlan::new`), so a leading object anchors inline in
    // that sentinel paragraph — no wrapper needed, and no spurious leading
    // boundary. (The earlier `emitted_paragraph` guard was overly cautious: it
    // predated confirming the sentinel provides the initial paragraph/style.)
    let collapse_bare_object_wrapper = has_objects && paragraph_is_text_empty;
    if !collapse_bare_object_wrapper {
      self.blocks.push(InputBlock::Paragraph(paragraph));
    }

    for image in images {
      self.blocks.push(InputBlock::Image(image));
      self.images_imported += 1;
    }

    for equation in equations {
      self.blocks.push(InputBlock::Equation(equation));
      self.equations_imported += 1;
    }
  }

  fn parse_table(&mut self, node: &XmlNode) -> InputTableBlock {
    self.tables_imported += 1;
    let column_widths = table_column_widths(node);
    let mut rows: Vec<InputTableRow> = Vec::new();
    let mut header_row = false;
    let mut first_row = true;
    // Widest row in grid columns, so the durable columns cover every cell's
    // grid position even when a row spans more columns than `tblGrid` declares.
    let mut grid_width = 0_usize;
    // grid column -> (row index, cell index) of the cell that started a vertical
    // merge, so continuation cells fold into its `row_span` instead of emitting.
    let mut vertical_open: FxHashMap<usize, (usize, usize)> = FxHashMap::default();

    for row_node in &node.children {
      if row_node.local.as_ref() != "tr" {
        continue;
      }
      if first_row {
        first_row = false;
        header_row = row_is_header(row_node);
      }
      // Deterministic row id, seeded per-table so row ids are GLOBALLY unique
      // across every table in the document. Cell identity is derived from
      // `(row_id, column_id)`, and cell text flows live in one global registry,
      // so two tables that reused a `(row, column)` coordinate would collide
      // their flows and lose text. A distinct per-table high-bit seed keeps
      // every cell coordinate unique while columns stay 1-based (below).
      let row_id = RowId(((self.tables_imported as u128) << 40) | (rows.len() as u128 + 1));
      let mut cells: Vec<InputTableCell> = Vec::new();
      let mut grid_col = 0_usize;
      for cell_node in &row_node.children {
        if cell_node.local.as_ref() == "tc" {
          grid_col = self.add_table_cell(cell_node, row_id, grid_col, &mut rows, &mut cells, &mut vertical_open);
        }
      }
      grid_width = grid_width.max(grid_col);
      rows.push(InputTableRow { id: row_id, cells });
    }

    let columns = build_table_columns(&column_widths, grid_width);
    fill_full_grid(&mut rows, &columns);
    InputTableBlock {
      rows,
      columns,
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
    row_id: RowId,
    grid_col: usize,
    rows: &mut [InputTableRow],
    cells: &mut Vec<InputTableCell>,
    vertical_open: &mut FxHashMap<usize, (usize, usize)>,
  ) -> usize {
    let col_span = cell_grid_span(cell_node);
    // The cell's durable column is the sequential id of its starting grid column.
    let column_id = ColumnId(grid_col as u128 + 1);
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
          id: CellId::from_coordinate(row_id, column_id),
          row_id,
          column_id,
          blocks,
          row_span: 1,
          col_span,
        });
      },
      VerticalMerge::None => {
        vertical_open.remove(&grid_col);
        let blocks = self.cell_blocks(cell_node);
        cells.push(InputTableCell {
          id: CellId::from_coordinate(row_id, column_id),
          row_id,
          column_id,
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
      match node.local.as_ref() {
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
    let alt_text = find_descendant(drawing, "docPr")
      .and_then(|doc_pr| doc_pr.attr("descr").or_else(|| doc_pr.attr("name")))
      .unwrap_or_default()
      .to_owned();
    if let Some(relationship_id) = find_descendant_with_attr(drawing, "blip", "embed").and_then(|blip| blip.attr("embed")) {
      let asset_id = self.images.resolve_image(relationship_id)?;
      return Some(InputImageBlock {
        asset_id,
        alt_text,
        sizing: drawing_sizing(drawing),
        alignment: InputBlockAlignment::Left,
        external_url: None,
      });
    }
    // §act-eleven A11.9: a link-only blip (`a:blip r:link`, no `r:embed`)
    // resolves through the external-mode relationship to a LINKED image.
    let link_id = find_descendant_with_attr(drawing, "blip", "link")?.attr("link")?;
    let (asset_id, url) = self.images.resolve_external_image(link_id)?;
    Some(InputImageBlock {
      asset_id,
      alt_text,
      sizing: drawing_sizing(drawing),
      alignment: InputBlockAlignment::Left,
      external_url: Some(url),
    })
  }
}

// -- Shared table assembly ----------------------------------------------------

/// Columns carry durable ids and widths; pad any grid position past the
/// declared widths with `Auto` so every cell's `column_id` resolves.
fn build_table_columns(column_widths: &[InputTableColumnWidth], grid_width: usize) -> Vec<InputTableColumn> {
  let column_count = column_widths.len().max(grid_width);
  (0..column_count)
    .map(|index| InputTableColumn {
      id: ColumnId(index as u128 + 1),
      width: column_widths
        .get(index)
        .cloned()
        .unwrap_or(InputTableColumnWidth::Auto),
    })
    .collect()
}

/// §P2b/FS-010: the canonical table grid is a FULL rectangle — the projection
/// requires one cell record at EVERY (row, column) coordinate. The `w:tc` walk
/// emits a cell only where a `<w:tc>` starts, so vMerge continuations,
/// `gridSpan`-covered columns, and short rows leave holes that
/// `table_topology::normalize` would otherwise report as `MissingCell` defects
/// (and a runtime repair pass would then have to synthesize). Fill those holes
/// here with the SAME empty placeholder the projection already synthesizes
/// (`CellId::from_coordinate`, span 1/1, one empty Normal paragraph) — the merge
/// stays carried solely by the anchor cell's `row_span`/`col_span`, so the
/// read-back grid, render, and docx export are all identical, minus the defect.
fn fill_full_grid(rows: &mut [InputTableRow], columns: &[InputTableColumn]) {
  for row in rows {
    let row_id = row.id;
    let mut present: FxHashMap<u128, InputTableCell> = row
      .cells
      .drain(..)
      .map(|cell| (cell.column_id.0, cell))
      .collect();
    row.cells = columns
      .iter()
      .map(|column| {
        present
          .remove(&column.id.0)
          .unwrap_or_else(|| InputTableCell {
            id: CellId::from_coordinate(row_id, column.id),
            row_id,
            column_id: column.id,
            blocks: vec![InputTableCellBlock::Paragraph(InputParagraph {
              style: ParagraphStyle::Normal,
              runs: Vec::new(),
            })],
            row_span: 1,
            col_span: 1,
          })
      })
      .collect();
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
    match child_node.local.as_ref() {
      // Strip the reserved U+FFFC object-placeholder sentinel from literal cell
      // text (see `soften_rdocx_breaks`) so it never reads back as an orphan.
      "t" => out.extend(child_node.text.chars().filter(|&ch| ch != '\u{FFFC}')),
      "tab" => out.push('\t'),
      // A docx <w:br>/<w:cr> is an INTRA-paragraph line break (Word Shift+Enter),
      // not a paragraph boundary. Emit the model's soft-break char (U+2028), never
      // '\n' — '\n' is the paragraph-separator in the body flow, so pushing it here
      // fabricates a bare boundary with no paragraph metadata/block/style record,
      // which the full reprojection then reports as missing_paragraph_* defects and
      // segments differently from the incremental projection (divergence). The
      // exporter already round-trips SOFT_LINE_BREAK back to a TextWrapping <w:br>.
      //
      // §perf-heaven T7.22: but only a LINE break (`w:type` absent or
      // `textWrapping`) is intra-paragraph content. `w:type="page"`/`"column"`
      // is pagination, which the model represents structurally (the
      // `pageBreakBefore` paragraph property), NOT as an inline char — mapping it
      // to U+2028 fabricated a spurious soft break in the text on import. The
      // model has no inline page-break, so a mid-run page/column break is dropped
      // (its surrounding text simply joins). This also disarms the exporter
      // landmine: were a Page break run ever emitted, reimport now drops it rather
      // than corrupting the text with a U+2028.
      "br" | "cr" => match child_node.attr("type") {
        Some("page") | Some("column") => {},
        _ => out.push(SOFT_LINE_BREAK),
      },
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
    .filter(|candidate| candidate.local.as_ref() == "gridCol")
    .map(|column| {
      column
        .attr("w")
        .and_then(|width| width.parse::<i64>().ok())
        .map_or(InputTableColumnWidth::Auto, column_width_from_twips)
    })
    .collect()
}

/// Twips → fixed pixel column width; non-positive or overflowing widths fall
/// back to `Auto` (shared by the typed and `XmlNode` walks).
fn column_width_from_twips(twips: i64) -> InputTableColumnWidth {
  Some(twips)
    .filter(|twips| *twips > 0)
    .and_then(|twips| u32::try_from(twips.saturating_mul(96) / 1440).ok())
    .map_or(InputTableColumnWidth::Auto, InputTableColumnWidth::FixedPx)
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

/// Typed-walk sizing from the parsed `wp:extent` EMUs; mirrors [`drawing_sizing`]
/// (a missing extent parses as 0 EMU, which maps to `Intrinsic`/`None` exactly
/// like a missing/non-positive attribute did).
fn sizing_from_extent_emu(extent_cx: i64, extent_cy: i64) -> InputImageSizing {
  let Some(width_px) = emu_value_to_px(extent_cx) else {
    return InputImageSizing::Intrinsic;
  };
  InputImageSizing::Fixed {
    width_px,
    height_px: emu_value_to_px(extent_cy),
  }
}

fn emu_to_px(value: &str) -> Option<u32> {
  emu_value_to_px(value.parse::<i64>().ok()?)
}

fn emu_value_to_px(emu: i64) -> Option<u32> {
  if emu <= 0 {
    return None;
  }
  u32::try_from(emu.saturating_mul(96) / 914_400).ok()
}

fn unescape_attribute_value(value: &str) -> String {
  quick_xml::escape::unescape(value)
    .map(Cow::into_owned)
    .unwrap_or_else(|_| value.to_owned())
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

// -- Lightweight DOM with byte spans (fallback path only) --------------------

/// A minimal element tree. `start..end` spans the element (from the opening `<`
/// of its start tag to just past its end tag) in the source XML, so equation
/// subtrees can be handed verbatim to [`omml`].
struct XmlNode {
  local: Cow<'static, str>,
  attrs: Vec<(Cow<'static, str>, String)>,
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
      .find(|entry| entry.0.as_ref() == local)
      .map(|entry| entry.1.as_str())
  }
}

/// §perf-heaven T8.1: intern an OOXML element/attribute local-name to a `'static`
/// reference. The structured walker builds one [`XmlNode`] per element in the body;
/// every node's `local` (and every attribute key) came from `String::to_owned`,
/// one heap allocation apiece across the whole tree. OOXML local names are a fixed
/// schema vocabulary, so the high-frequency ones (every run's `r`/`t`/`rPr` and its
/// formatting children, every paragraph's `p`/`pPr`) map to a `'static` string with
/// no allocation. Unknown names (rare, non-schema) fall back to an owned copy —
/// correct, just not allocation-free. Netted by the corpus import-fidelity sweep
/// (the projection must stay byte-identical).
fn intern_local(name: &str) -> Cow<'static, str> {
  // Ordered roughly by body frequency; the `match` compiles to length-bucketed
  // comparisons, so the hot run/paragraph names resolve in a few byte checks.
  let interned: Option<&'static str> = match name {
    "r" => Some("r"),
    "t" => Some("t"),
    "rPr" => Some("rPr"),
    "p" => Some("p"),
    "pPr" => Some("pPr"),
    "rFonts" => Some("rFonts"),
    "sz" => Some("sz"),
    "szCs" => Some("szCs"),
    "color" => Some("color"),
    "highlight" => Some("highlight"),
    "b" => Some("b"),
    "bCs" => Some("bCs"),
    "i" => Some("i"),
    "iCs" => Some("iCs"),
    "u" => Some("u"),
    "shd" => Some("shd"),
    "jc" => Some("jc"),
    "spacing" => Some("spacing"),
    "ind" => Some("ind"),
    "vertAlign" => Some("vertAlign"),
    "lang" => Some("lang"),
    "noProof" => Some("noProof"),
    "pStyle" => Some("pStyle"),
    "rStyle" => Some("rStyle"),
    "br" => Some("br"),
    "tab" => Some("tab"),
    "bookmarkStart" => Some("bookmarkStart"),
    "bookmarkEnd" => Some("bookmarkEnd"),
    "hyperlink" => Some("hyperlink"),
    "body" => Some("body"),
    "tbl" => Some("tbl"),
    "tr" => Some("tr"),
    "tc" => Some("tc"),
    "tcPr" => Some("tcPr"),
    "trPr" => Some("trPr"),
    "tblPr" => Some("tblPr"),
    "tblGrid" => Some("tblGrid"),
    "gridCol" => Some("gridCol"),
    "gridSpan" => Some("gridSpan"),
    "vMerge" => Some("vMerge"),
    "tcW" => Some("tcW"),
    "drawing" => Some("drawing"),
    "oMath" => Some("oMath"),
    "oMathPara" => Some("oMathPara"),
    "blip" => Some("blip"),
    "docPr" => Some("docPr"),
    "inline" => Some("inline"),
    "anchor" => Some("anchor"),
    "extent" => Some("extent"),
    "sdt" => Some("sdt"),
    "sdtContent" => Some("sdtContent"),
    "sdtPr" => Some("sdtPr"),
    "sectPr" => Some("sectPr"),
    "numPr" => Some("numPr"),
    _ => None,
  };
  match interned {
    Some(known) => Cow::Borrowed(known),
    None => Cow::Owned(name.to_owned()),
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
          && let Ok(text) = event.xml10_content()
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
  let local = intern_local(local_name(event.name().as_ref()));
  let mut attrs = Vec::new();
  for attribute in event.attributes().flatten() {
    let key = intern_local(local_name(attribute.key.as_ref()));
    // §perf-heaven T7.20: XML-unescape the attribute value (quick-xml does NOT
    // auto-unescape attributes, unlike the `Event::Text` path). Without this,
    // an image's `docPr@descr` alt-text containing `&#xA;` (Word's multi-line
    // descriptions) leaked the literal characters `&#xA;` into body text via the
    // `[alt]` image fallback. `escape::unescape` decodes the entities directly
    // (no `Decoder` needed); fall back to the raw string only if unescape fails.
    let raw = String::from_utf8_lossy(attribute.value.as_ref());
    let value = quick_xml::escape::unescape(&raw)
      .map(|value| value.into_owned())
      .unwrap_or_else(|_| raw.into_owned());
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
    .find(|candidate| candidate.local.as_ref() == local)
}

fn find_descendant<'tree>(node: &'tree XmlNode, local: &str) -> Option<&'tree XmlNode> {
  for candidate in &node.children {
    if candidate.local.as_ref() == local {
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
    if candidate.local.as_ref() == local && candidate.attr(attr).is_some() {
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
    if candidate.local.as_ref() == local {
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

  fn context(body: &str) -> (CleanedDocx, CT_Document) {
    let xml = doc_xml(body);
    let document = CT_Document::from_xml(&xml).expect("typed main-document parse");
    let cleaned = CleanedDocx {
      bytes: Vec::new(),
      main_document_xml: Some(std::sync::Arc::from(xml)),
      package: None,
      report: crate::cleaner::DocxCleanReport {
        stats: crate::cleaner::DocxCleanStats::default(),
        actions: crate::cleaner::CLEANING_RULES,
      },
    };
    (cleaned, document)
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

  /// §act-eleven C3: typed-vs-fallback equivalence over representative
  /// constructs. The old `XmlNode` walk is the typed walk's correctness escape
  /// hatch; this pins (a) both walks producing IDENTICAL output on the shapes
  /// the typed walk claims, and (b) the parity pre-scan actually ROUTING these
  /// shapes to the typed walk (so a probe regression that silently sends
  /// everything to the fallback fails loudly here, not as a perf mystery).
  #[test]
  fn typed_walk_matches_fallback_walk_on_representative_bodies() {
    let fixtures: &[(&str, &str, &[&str])] = &[
      (
        "plain paragraphs",
        r"<w:p><w:r><w:t>alpha</w:t></w:r></w:p><w:p><w:r><w:t>beta</w:t></w:r></w:p>",
        &["alpha", "beta"],
      ),
      (
        "self-closed empty paragraph between text",
        r"<w:p><w:r><w:t>alpha</w:t></w:r></w:p><w:p/><w:p><w:r><w:t>beta</w:t></w:r></w:p>",
        &["alpha", "", "beta"],
      ),
      (
        "table with header, spans, merges, soft break + tab cell text",
        concat!(
          r#"<w:p><w:r><w:t>intro</w:t></w:r></w:p>"#,
          r#"<w:tbl><w:tblGrid><w:gridCol w:w="1440"/><w:gridCol w:w="2880"/></w:tblGrid>"#,
          r#"<w:tr><w:trPr><w:tblHeader/></w:trPr><w:tc><w:tcPr><w:gridSpan w:val="2"/></w:tcPr><w:p><w:r><w:t>head</w:t></w:r></w:p></w:tc></w:tr>"#,
          r#"<w:tr><w:tc><w:tcPr><w:vMerge w:val="restart"/></w:tcPr><w:p><w:r><w:t>a</w:t><w:br/><w:t>b</w:t><w:tab/><w:t>c</w:t></w:r></w:p></w:tc>"#,
          r#"<w:tc><w:p><w:r><w:t>right</w:t></w:r></w:p></w:tc></w:tr>"#,
          r#"<w:tr><w:tc><w:tcPr><w:vMerge/></w:tcPr><w:p/></w:tc><w:tc><w:p><w:r><w:t>tail</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#
        ),
        &["intro"],
      ),
      (
        "bare equation wrapper collapses in both walks",
        r"<w:p><w:r><w:t>before</w:t></w:r></w:p><w:p><m:oMath><m:r><m:t>x+1</m:t></m:r></m:oMath></w:p><w:p><w:r><w:t>after</w:t></w:r></w:p>",
        &["before", "", "after"],
      ),
      (
        "nested table in a cell",
        concat!(
          r#"<w:tbl><w:tblGrid><w:gridCol w:w="1440"/></w:tblGrid><w:tr><w:tc>"#,
          r#"<w:p><w:r><w:t>outer</w:t></w:r></w:p>"#,
          r#"<w:tbl><w:tblGrid><w:gridCol w:w="720"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>inner</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#,
          r#"</w:tc></w:tr></w:tbl>"#
        ),
        &[],
      ),
    ];
    for (label, body, slice_texts) in fixtures {
      let (cleaned, document) = context(body);
      let paragraphs: Vec<DocumentParagraphInput> = slice_texts.iter().map(|text| normal(text)).collect();
      let xml = cleaned.main_document_xml.clone().expect("fixture xml");
      assert!(
        !typed_walk_diverges(&document, &xml, paragraphs.len()),
        "{label}: the parity pre-scan must route this shape to the TYPED walk (coverage regression)"
      );
      let typed = interpret_structured(&cleaned, &document, &paragraphs).expect("typed walk");
      let fallback = interpret_structured_via_tree(&cleaned, &xml, &paragraphs).expect("fallback walk");
      assert_eq!(typed.blocks, fallback.blocks, "{label}: typed and fallback walks diverged on blocks");
      assert_eq!(typed.tables_imported, fallback.tables_imported, "{label}: table count diverged");
      assert_eq!(typed.images_imported, fallback.images_imported, "{label}: image count diverged");
      assert_eq!(typed.equations_imported, fallback.equations_imported, "{label}: equation count diverged");
    }
  }

  #[test]
  fn table_with_paragraph_cell_is_emitted_in_body_order() {
    let body = r#"<w:p><w:r><w:t>intro</w:t></w:r></w:p><w:tbl><w:tblGrid><w:gridCol w:w="1440"/><w:gridCol w:w="2880"/></w:tblGrid><w:tr><w:trPr><w:tblHeader/></w:trPr><w:tc><w:p><w:r><w:t>left</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>right</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#;
    let (cleaned, document) = context(body);
    let paragraphs = [normal("intro")];

    let structured = interpret_structured(&cleaned, &document, &paragraphs).expect("structured import");

    assert_eq!(structured.tables_imported, 1);
    assert!(matches!(structured.blocks.first(), Some(InputBlock::Paragraph(_))));
    let Some(InputBlock::Table(table)) = structured.blocks.get(1) else {
      panic!("expected a table block after the intro paragraph");
    };
    assert!(table.style.header_row);
    assert_eq!(
      table.columns,
      vec![
        InputTableColumn {
          id: ColumnId(1),
          width: InputTableColumnWidth::FixedPx(96),
        },
        InputTableColumn {
          id: ColumnId(2),
          width: InputTableColumnWidth::FixedPx(192),
        },
      ]
    );
    assert_eq!(table.rows.len(), 1);
    assert_eq!(table.rows[0].cells.len(), 2);
    let InputTableCellBlock::Paragraph(left) = &table.rows[0].cells[0].blocks[0] else {
      panic!("expected a paragraph in the first cell");
    };
    assert_eq!(left.runs[0].text, "left");
  }

  #[test]
  fn vertical_merge_carries_span_and_fills_continuation_cell() {
    let body = r#"<w:tbl><w:tblGrid><w:gridCol w:w="1440"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:vMerge w:val="restart"/></w:tcPr><w:p><w:r><w:t>top</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:tcPr><w:vMerge/></w:tcPr><w:p/></w:tc></w:tr></w:tbl>"#;
    let (cleaned, document) = context(body);

    let structured = interpret_structured(&cleaned, &document, &[]).expect("structured import");

    let Some(InputBlock::Table(table)) = structured.blocks.first() else {
      panic!("expected a table block");
    };
    assert_eq!(table.rows.len(), 2);
    // The merge is carried solely by the anchor cell's row_span...
    assert_eq!(table.rows[0].cells.len(), 1);
    assert_eq!(table.rows[0].cells[0].row_span, 2);
    // ...and the continuation coordinate now gets a FULL-GRID placeholder cell
    // (empty, span 1/1) so the projection reads a complete rectangle with no
    // MissingCell defect (§P2b/FS-010). Merge semantics are unchanged.
    assert_eq!(table.rows[1].cells.len(), 1);
    let continuation = &table.rows[1].cells[0];
    assert_eq!(continuation.row_span, 1);
    assert_eq!(continuation.col_span, 1);
    assert_eq!(continuation.column_id, ColumnId(1));
    let InputTableCellBlock::Paragraph(placeholder) = &continuation.blocks[0] else {
      panic!("continuation placeholder should be an empty paragraph");
    };
    assert!(placeholder.runs.is_empty(), "continuation placeholder is empty");
  }

  #[test]
  fn inline_office_math_emits_equation_after_paragraph() {
    let body = r"<w:p><w:r><w:t>see</w:t></w:r><m:oMath><m:f><m:num><m:r><m:t>1</m:t></m:r></m:num><m:den><m:r><m:t>2</m:t></m:r></m:den></m:f></m:oMath></w:p>";
    let (cleaned, document) = context(body);
    let paragraphs = [normal("see")];

    let structured = interpret_structured(&cleaned, &document, &paragraphs).expect("structured import");

    assert_eq!(structured.equations_imported, 1);
    assert!(matches!(structured.blocks.first(), Some(InputBlock::Paragraph(_))));
    let Some(InputBlock::Equation(equation)) = structured.blocks.get(1) else {
      panic!("expected an equation block after the paragraph");
    };
    assert_eq!(equation.source, "\\frac{1}{2}");
  }

  #[test]
  fn body_without_objects_reports_zero_counts() {
    let (cleaned, document) = context(r"<w:p><w:r><w:t>plain</w:t></w:r></w:p>");
    let structured = interpret_structured(&cleaned, &document, &[normal("plain")]).expect("structured import");

    assert_eq!(structured.tables_imported, 0);
    assert_eq!(structured.images_imported, 0);
    assert_eq!(structured.equations_imported, 0);
    assert!(structured.assets.is_empty());
    assert_eq!(structured.blocks.len(), 1);
  }
}
