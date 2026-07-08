use std::{collections::BTreeMap, io, sync::Arc};

use flowstate_fidelity::{self as fidelity, FidelityClass};
use gpui_flowtext::{
  AssetId, BlockId, CellId, DocumentProjection, DocumentSection, DocumentTheme, HighlightStyle, InputBlock, InputBlockAlignment,
  InputEquationBlock, InputEquationDisplay, InputEquationSyntax, InputImageBlock, InputImageSizing, InputParagraph, InputRun, InputTableBlock,
  InputTableCell, InputTableCellBlock, InputTableColumn, InputTableColumnWidth, InputTableRow, InputTableStyle, ParagraphId, RunSemanticStyle,
  RunStyles, SectionId, SectionKind, document_from_input_blocks,
};
use loro::{Container, ContainerID, ContainerTrait, ID, LoroDoc, LoroMap, LoroText, LoroValue, ValueOrContainer, cursor::{Cursor, Side}};
use rustc_hash::FxHashMap;

use crate::{
  BLOCKS_BY_ID, FLOW_TEXT_KEY, FLOWS_BY_ID, MAIN_BODY_BLOCK_ID, MARK_DIRECT_UNDERLINE, MARK_HIGHLIGHT_STYLE, MARK_PARAGRAPH_STYLE,
  MARK_RUN_SEMANTIC_STYLE, MARK_STRIKETHROUGH, OBJECT_REPLACEMENT, PARAGRAPHS_BY_ID, ROOT, ROOT_BODY_FLOW_ID, ROOT_FIRST_PARAGRAPH_ID,
  SECTIONS_BY_ID, TABLE_CELLS_BY_ID, TABLE_COLUMN_ORDER, TABLE_COLUMNS_BY_ID, TABLE_KEY, TABLE_ROW_ORDER, flowstate_document_theme,
  parse_cell_loro_id, parse_column_loro_id, parse_row_loro_id, table_topology,
  projection_defects::{ProjectionDefect, TableTopologyKind},
};

pub fn document_from_loro(doc: &LoroDoc) -> io::Result<DocumentProjection> {
  // Callers that cannot repair still get the deterministic projection; the
  // defects are simply discarded here (the runtime uses the sibling below).
  Ok(document_from_loro_with_defects(doc)?.0)
}

/// §5: project the document and report every malformed-canonical-state defect
/// encountered on the way. The projection is deterministic even when defective:
/// unresolvable blocks are quarantined (appended in stable order) instead of
/// dropped, and fabricated identities are deterministic per projection.
#[hotpath::measure]
pub fn document_from_loro_with_defects(doc: &LoroDoc) -> io::Result<(DocumentProjection, Vec<ProjectionDefect>)> {
  crate::instrument::record_full_projection();
  let mut defects = Vec::new();
  let projection = projection_from_loro_with_defects(doc, &mut defects)?;
  let mut document = document_from_projection_blocks(projection);
  document.frontier = doc.state_frontiers().encode();
  Ok((document, defects))
}

/// Rows produced by a REGIONAL rematerialization (spec §6-R): the same
/// materializer law as [`document_from_loro`], applied to one body region.
pub struct RegionRows {
  pub blocks: Vec<InputBlock>,
  pub paragraph_ids: Vec<ParagraphId>,
  pub block_ids: Vec<BlockId>,
  pub defects: Vec<ProjectionDefect>,
}

/// Spec §6-R: materialize the body rows covering `[sentinel_unicode, end_unicode)`
/// — a region that STARTS at a row's leading boundary sentinel (`\n`, or the seed
/// sentinel at 0) and ENDS exclusively at the next retained row's leading sentinel
/// (or the body end). Runs the SAME flow walk as the full materialization
/// ([`document_from_loro`]) over a `slice_delta` of the region, so coalescing,
/// style defaults, fabrication, and defect reporting are one law, not a copy.
///
/// Callers supply the identity context, all keyed by ABSOLUTE flow positions:
/// boundary→record-key maps for the candidate paragraph/paragraph-block records
/// and the region's resolved object blocks. Quarantine append and the empty-doc
/// placeholder are full-rebuild concerns and intentionally do not apply here.
#[allow(clippy::implicit_hasher, reason = "the maps are shared with the internal flow walker, whose boundary indexes are FxHashMap by construction")]
pub fn materialize_body_region(
  doc: &LoroDoc,
  sentinel_unicode: usize,
  end_unicode: usize,
  paragraph_ids_by_boundary: &FxHashMap<usize, String>,
  paragraph_block_ids_by_boundary: &FxHashMap<usize, String>,
  object_blocks_by_pos: &BTreeMap<usize, LoroMap>,
) -> io::Result<RegionRows> {
  let projector = Projector::new(doc)?;
  let body = projector.flow_text(ROOT_BODY_FLOW_ID)?;
  let end = end_unicode.min(body.len_unicode());
  if sentinel_unicode >= end {
    return Err(invalid("regional rematerialization given an empty region"));
  }
  let delta = body
    .slice_delta(sentinel_unicode, end, loro::cursor::PosType::Unicode)
    .map_err(|error| invalid(format!("regional slice_delta failed: {error}")))?;
  let mut blocks = Vec::new();
  let mut paragraph_ids = Vec::new();
  let mut block_ids = Vec::new();
  let mut defects = Vec::new();
  Projector::walk_flow_delta(
    &body,
    delta,
    sentinel_unicode,
    object_blocks_by_pos,
    ROOT_BODY_FLOW_ID,
    Some(paragraph_ids_by_boundary),
    Some(paragraph_block_ids_by_boundary),
    Some(&mut paragraph_ids),
    Some(&mut block_ids),
    &mut blocks,
    &mut defects,
    false,
    |block, defects| projector.object_block(block, defects),
  )?;
  Ok(RegionRows {
    blocks,
    paragraph_ids,
    block_ids,
    defects,
  })
}

/// §act-four M4 (cold viewport load): materialize just the body rows covering
/// `[start_unicode, end_unicode)` from a cold-loaded document, WITHOUT building
/// the whole projection. Content materialization is `O(viewport)` (the §6-R
/// `slice_delta` region walk); the boundary→id maps reuse the SAME assembly the
/// full [`document_from_loro`] uses, so the output is byte-identical to the
/// corresponding slice of the full rebuild. `start_unicode` is snapped DOWN to
/// the nearest row-leading boundary (the region walk must start at a sentinel);
/// pass `[0, body_len)` for the whole doc, or a viewport for scroll.
pub fn materialize_viewport(doc: &LoroDoc, start_unicode: usize, end_unicode: usize) -> io::Result<RegionRows> {
  let projector = Projector::new(doc)?;
  let body = projector.flow_text(ROOT_BODY_FLOW_ID)?;
  // Metadata-only maps (no content materialization) — the same functions the
  // full projection resolves once per flow.
  let paragraph_map = paragraph_ids_by_boundary(doc, &body);
  let pblock_map = paragraph_block_ids_by_boundary(doc, &body);
  let mut defects = Vec::new();
  let (object_map, _quarantined) = projector.object_blocks_for_flow(&body, ROOT_BODY_FLOW_ID, &mut defects)?;
  // Snap the start to the row-leading boundary at or before it (0 covers the
  // seed sentinel), so the region walk begins at a sentinel as required.
  let sentinel = paragraph_map.keys().copied().filter(|boundary| *boundary <= start_unicode).max().unwrap_or(0);
  let end = end_unicode.max(sentinel + 1).min(body.len_unicode());
  materialize_body_region(doc, sentinel, end, &paragraph_map, &pblock_map, &object_map)
}

/// §act-four M4 (cold scroll): the body-unicode position of every block's
/// leading boundary, in block order — paragraph leading `\n`s plus object
/// U+FFFC placeholders, sorted + deduped. Persisted in the package manifest so
/// a cold open maps a block-index viewport to a unicode range in `O(1)` (no
/// per-open `O(records)` boundary scan) before calling [`materialize_viewport`].
/// `boundaries[i]` is block `i`'s leading position; a viewport `[a, b)` decodes
/// `[boundaries[a], boundaries[b])`. `O(records)` — metadata only, no content.
pub fn body_block_boundaries(doc: &LoroDoc) -> io::Result<Vec<u32>> {
  let projector = Projector::new(doc)?;
  let body = projector.flow_text(ROOT_BODY_FLOW_ID)?;
  let mut boundaries: Vec<usize> = paragraph_ids_by_boundary(doc, &body).into_keys().collect();
  let mut defects = Vec::new();
  let (objects, _quarantined) = projector.object_blocks_for_flow(&body, ROOT_BODY_FLOW_ID, &mut defects)?;
  boundaries.extend(objects.keys().copied());
  boundaries.sort_unstable();
  boundaries.dedup();
  Ok(boundaries.into_iter().map(|position| u32::try_from(position).unwrap_or(u32::MAX)).collect())
}

pub(crate) fn input_blocks_from_loro(doc: &LoroDoc) -> io::Result<Vec<InputBlock>> {
  Ok(projection_from_loro(doc)?.blocks)
}

pub fn object_input_blocks_from_loro(doc: &LoroDoc) -> io::Result<Vec<(BlockId, InputBlock)>> {
  let projector = Projector::new(doc)?;
  let mut blocks = Vec::new();
  let mut defect_sink = Vec::new();
  for key in projector.blocks.keys().map(|key| key.to_string()) {
    let Some(block) = child_map(&projector.blocks, &key)? else {
      continue;
    };
    if map_string_opt(&block, "kind")?.as_deref() == Some("paragraph") {
      continue;
    }
    let id = map_string_opt(&block, "id")?.unwrap_or(key);
    blocks.push((BlockId(loro_id_u128(&id)), projector.object_block(&block, &mut defect_sink)?));
  }
  blocks.sort_by_key(|(id, _)| id.0);
  Ok(blocks)
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct ProjectionBlocks {
  #[serde(default)]
  pub document_id: u128,
  pub blocks: Vec<InputBlock>,
  pub paragraph_ids: Vec<ParagraphId>,
  pub block_ids: Vec<BlockId>,
  #[serde(default)]
  pub sections: Vec<DocumentSection>,
}

fn projection_from_loro(doc: &LoroDoc) -> io::Result<ProjectionBlocks> {
  let mut defects = Vec::new();
  projection_from_loro_with_defects(doc, &mut defects)
}

fn projection_from_loro_with_defects(doc: &LoroDoc, defects: &mut Vec<ProjectionDefect>) -> io::Result<ProjectionBlocks> {
  let projector = Projector::new(doc)?;
  projector.body_projection(defects)
}

pub(crate) fn projection_blocks_from_loro(doc: &LoroDoc) -> io::Result<ProjectionBlocks> {
  projection_from_loro(doc)
}

pub(crate) fn document_from_projection_blocks(projection: ProjectionBlocks) -> DocumentProjection {
  let mut document = document_from_input_blocks(DocumentTheme::clone(&flowstate_document_theme()), projection.blocks);
  if projection.document_id != 0 {
    document.ids.document_id = projection.document_id;
  }
  if projection.paragraph_ids.len() == document.paragraphs.len() {
    document.ids.paragraph_ids = projection.paragraph_ids;
  }
  if projection.block_ids.len() == document.blocks.len() {
    document.ids.block_ids = projection.block_ids;
  }
  if !projection.sections.is_empty() {
    document.sections = Arc::new(projection.sections);
  }
  document
}

struct Projector<'a> {
  doc: &'a LoroDoc,
  flows: LoroMap,
  blocks: LoroMap,
}

impl<'a> Projector<'a> {
  fn new(doc: &'a LoroDoc) -> io::Result<Self> {
    let root = doc.get_map(ROOT);
    let flows = child_map(&root, FLOWS_BY_ID)?.ok_or_else(|| invalid("Flowstate Loro package has no flows map"))?;
    let blocks = child_map(&root, BLOCKS_BY_ID)?.ok_or_else(|| invalid("Flowstate Loro package has no block registry"))?;
    Ok(Self { doc, flows, blocks })
  }

  fn body_projection(&self, defects: &mut Vec<ProjectionDefect>) -> io::Result<ProjectionBlocks> {
    let body = self.flow_text(ROOT_BODY_FLOW_ID)?;
    let (body_blocks, quarantined) = self.object_blocks_for_flow(&body, ROOT_BODY_FLOW_ID, defects)?;
    let mut blocks = Vec::new();
    let mut paragraph_ids = Vec::new();
    let mut block_ids = Vec::new();
    self.push_flow_blocks(&body, &body_blocks, ROOT_BODY_FLOW_ID, Some(&mut paragraph_ids), Some(&mut block_ids), &mut blocks, defects, true)?;
    // §5 quarantine: blocks whose anchors no longer resolve are appended at the
    // end in stable (sorted block key) order instead of vanishing. Their defects
    // were already recorded by `object_blocks_for_flow`.
    for (key, block) in quarantined {
      if let Ok(projected) = self.object_block(&block, defects) {
        let id = map_string_opt(&block, "id")?.unwrap_or(key);
        blocks.push(projected);
        block_ids.push(BlockId(loro_id_u128(&id)));
      }
    }
    if paragraph_ids.is_empty() {
      // No paragraph rows at all — either a truly empty projection, or a body
      // ending in object rows only (e.g. an object inserted into an empty
      // document). The editor's document assembly appends a mandatory trailing
      // paragraph in both shapes; emitting it HERE, with a deterministic
      // fabricated identity, keeps `document_from_loro` a pure function — the
      // silent length-mismatch fallback in `document_from_projection_blocks`
      // previously let the assembly mint a RANDOM id for that row, so two
      // rebuilds of the same doc disagreed (found by the object-fuzz undo arm).
      blocks.push(InputBlock::Paragraph(InputParagraph {
        style: gpui_flowtext::ParagraphStyle::Normal,
        runs: Vec::new(),
      }));
      // §5/FS-004: even the deterministic empty-projection placeholder is a
      // fabricated identity; report it so the runtime seeds durable state.
      defects.push(ProjectionDefect::MissingParagraphMetadata {
        flow_id: ROOT_BODY_FLOW_ID.to_string(),
        boundary_unicode: None,
        fabricated_id: loro_id_u128("paragraph.projection.empty"),
      });
      paragraph_ids.push(ParagraphId(loro_id_u128("paragraph.projection.empty")));
      block_ids.push(BlockId(loro_id_u128("block.projection.empty")));
    }
    // §5 fidelity: surface every collected projection defect (Structure firehose)
    // and assert the canonical body-projection invariants. Fully read-only and
    // gated, so it costs a single atomic load when tracing is disabled.
    if fidelity::enabled() {
      for defect in defects.iter() {
        fidelity::event(FidelityClass::Structure, "defect", || format!("{} @ {}", defect.class(), defect.stable_key()));
      }
      check_body_projection_integrity(&body, &body_blocks, defects.as_slice());
    }
    let sections = self.sections_for_projection(&paragraph_ids)?;
    Ok(ProjectionBlocks {
      document_id: crate::loro_schema::document_id(self.doc).map_or(0, |id| id.as_u128()),
      blocks,
      paragraph_ids,
      block_ids,
      sections,
    })
  }

  /// Project a `DocumentSection` for each Loro section, including its §11 page
  /// structure (page size, margins, columns, orientation, page numbering,
  /// header/footer flow ids). The canonical attrs live in Loro and are read
  /// back via [`crate::loro_schema::read_section_page_attrs`] (which substitutes
  /// documented defaults for missing keys), then mapped field-for-field onto the
  /// gpui-flowtext projection mirror `gpui_flowtext::SectionPageAttrs`.
  #[hotpath::measure]
  fn sections_for_projection(&self, paragraph_ids: &[ParagraphId]) -> io::Result<Vec<DocumentSection>> {
    let root = self.doc.get_map(ROOT);
    let Some(sections_by_id) = child_map(&root, SECTIONS_BY_ID)? else {
      return Ok(Vec::new());
    };
    // §perf: this index is only ever point-queried (section ordering comes from the
    // explicit sort_by_key below), so an FxHashMap avoids the red-black-tree build.
    let paragraph_order = paragraph_ids
      .iter()
      .enumerate()
      .map(|(ix, id)| (id.0, ix))
      .collect::<FxHashMap<_, _>>();
    let mut sections = Vec::new();
    for key in map_keys(&sections_by_id) {
      let Some(section) = child_map(&sections_by_id, &key)? else {
        continue;
      };
      let Some(start_paragraph) = section_id_field(&section, "start_paragraph_id")? else {
        continue;
      };
      let section_id = map_string_opt(&section, "id")?
        .and_then(|value| parse_u128(&value))
        .unwrap_or_else(|| loro_id_u128(&key));
      let kind_slot = map_i64_opt(&section, "kind_slot")?
        .and_then(i64_to_u8)
        .unwrap_or(0);
      // §11: read the section's canonical page attrs from its `attrs` child map
      // (defaults substituted for missing keys) and project them. The section
      // map always exists here, so `page` is always `Some(..)` for determinism.
      let canonical_page = match child_map(&section, "attrs")? {
        Some(attrs) => crate::loro_schema::read_section_page_attrs(&attrs),
        None => crate::loro_schema::SectionPageAttrs::default(),
      };
      sections.push(DocumentSection {
        id: SectionId(section_id),
        parent_id: section_id_field(&section, "parent_section_id")?.map(SectionId),
        kind: SectionKind::Custom(kind_slot),
        heading_paragraph: section_id_field(&section, "heading_paragraph_id")?.map(ParagraphId),
        start_paragraph: ParagraphId(start_paragraph),
        end_paragraph_exclusive: section_id_field(&section, "end_paragraph_exclusive_id")?.map(ParagraphId),
        page: Some(project_section_page_attrs(canonical_page)),
      });
    }
    sections.sort_by_key(|section| {
      paragraph_order
        .get(&section.start_paragraph.0)
        .copied()
        .unwrap_or(usize::MAX)
    });
    Ok(sections)
  }

  #[allow(clippy::too_many_arguments, reason = "projection threading requires flow context, id pools, output and defect sink together")]
  #[allow(clippy::too_many_arguments, reason = "projection threading requires flow context, id pools, output and defect sink together")]
  fn push_flow_blocks(
    &self,
    text: &LoroText,
    object_blocks: &BTreeMap<usize, LoroMap>,
    flow_id: &str,
    paragraph_ids: Option<&mut Vec<ParagraphId>>,
    block_ids: Option<&mut Vec<BlockId>>,
    output: &mut Vec<InputBlock>,
    defects: &mut Vec<ProjectionDefect>,
    flush_trailing_after_object: bool,
  ) -> io::Result<()> {
    // §perf: resolve this flow's boundary→id maps ONCE (only for the id sinks that
    // are actually collected — cell flows pass `None` and skip the build), then do
    // O(1) lookups per boundary below. Replaces a per-boundary full rescan of every
    // paragraph/block record — an O(records²·chars) blow-up that pegged the CRDT
    // actor thread at 100% CPU and never returned when materializing a large document.
    let paragraph_index = paragraph_ids.as_ref().map(|_| paragraph_ids_by_boundary(self.doc, text));
    let paragraph_block_index = block_ids.as_ref().map(|_| paragraph_block_ids_by_boundary(self.doc, text));

    let delta = hotpath::measure_block!("projector_body_to_delta", text.to_delta());
    hotpath::measure_block!("projector_walk_flow_delta", Self::walk_flow_delta(
      text,
      delta,
      0,
      object_blocks,
      flow_id,
      paragraph_index.as_ref(),
      paragraph_block_index.as_ref(),
      paragraph_ids,
      block_ids,
      output,
      defects,
      flush_trailing_after_object,
      |block, defects| self.object_block(block, defects),
    ))
  }

  /// Core flow walk shared by the FULL materialization ([`Self::push_flow_blocks`])
  /// and the REGIONAL rematerialization ([`materialize_body_region`], spec §6-R):
  /// ONE implementation of the paragraph/object/coalescing/defect law, applied to
  /// either the whole flow delta from position 0 or a `slice_delta` region that
  /// starts at a row's leading boundary sentinel. `unicode_pos` runs in ABSOLUTE
  /// flow coordinates either way, so boundary-id maps and object positions are
  /// always absolute.
  #[allow(clippy::too_many_arguments, reason = "projection threading requires flow context, id pools, output and defect sink together")]
  fn walk_flow_delta(
    text: &LoroText,
    delta: Vec<loro::TextDelta>,
    start_unicode: usize,
    object_blocks: &BTreeMap<usize, LoroMap>,
    flow_id: &str,
    paragraph_index: Option<&FxHashMap<usize, String>>,
    paragraph_block_index: Option<&FxHashMap<usize, String>>,
    mut paragraph_ids: Option<&mut Vec<ParagraphId>>,
    mut block_ids: Option<&mut Vec<BlockId>>,
    output: &mut Vec<InputBlock>,
    defects: &mut Vec<ProjectionDefect>,
    flush_trailing_after_object: bool,
    mut project_object: impl FnMut(&LoroMap, &mut Vec<ProjectionDefect>) -> io::Result<InputBlock>,
  ) -> io::Result<()> {
    let mut current = InputParagraph {
      style: gpui_flowtext::ParagraphStyle::Normal,
      runs: Vec::new(),
    };
    // Durable key of the most recently emitted object row: the identity
    // anchor for any boundary-less (interstitial/trailing) paragraph row that
    // follows it (see `push_paragraph_projection_metadata`).
    let mut last_object_key: Option<String> = None;
    let mut pending_style = gpui_flowtext::ParagraphStyle::Normal;
    let mut seen_sentinel = false;
    let mut unicode_pos = start_unicode;
    let mut current_boundary = None;

    for item in delta {
      let loro::TextDelta::Insert { insert, attributes } = item else {
        continue;
      };
      let run_styles = run_styles_from_attrs(attributes.as_ref());
      for ch in insert.chars() {
        match ch {
          '\n' => {
            // §5 (adjustmentplan:224): a paragraph boundary without a
            // paragraph-style mark projects as Normal and is reported so the
            // runtime repairs the canonical mark, instead of silently
            // inheriting the previous paragraph's style.
            let style = match paragraph_style_from_attrs(attributes.as_ref()) {
              Some(style) => style,
              None => {
                defects.push(ProjectionDefect::MissingParagraphStyleMark {
                  flow_id: flow_id.to_string(),
                  boundary_unicode: unicode_pos,
                });
                gpui_flowtext::ParagraphStyle::Normal
              },
            };
            if !seen_sentinel {
              seen_sentinel = true;
              pending_style = style;
              current.style = style;
              current_boundary = Some(unicode_pos);
            } else if current.runs.is_empty()
              && current_boundary.is_none()
              && output
                .last()
                .is_some_and(|block| !matches!(block, InputBlock::Paragraph(_)))
            {
              // Fork B (docs/collab-coalescing-parity.md): coalesce ONLY the phantom
              // empty paragraph that an object's own block implies — the first `\n`
              // after an object, when `current_boundary` is still None because the
              // object reset it. A SUBSEQUENT empty `\n` (current_boundary already Some)
              // is a REAL, user/edit-created empty paragraph with a durable record;
              // KEEP it so an empty line next to an image survives instead of being
              // silently dropped. (The incremental replay's matching half is still in
              // progress — see the doc; the structural fuzz stays #[ignore]d until then.)
              current.style = style;
              pending_style = style;
              current_boundary = Some(unicode_pos);
            } else {
              push_paragraph_projection_metadata(
                text,
                paragraph_index,
                paragraph_block_index,
                flow_id,
                current_boundary,
                output.len(),
                last_object_key.as_deref(),
                paragraph_ids.as_deref_mut(),
                block_ids.as_deref_mut(),
                defects,
              );
              output.push(InputBlock::Paragraph(current));
              current = InputParagraph { style, runs: Vec::new() };
              pending_style = style;
              current_boundary = Some(unicode_pos);
            }
          },
          OBJECT_REPLACEMENT => {
            if let Some(block) = object_blocks.get(&unicode_pos) {
              // Fork B symmetry (docs/collab-coalescing-parity.md): flush a REAL empty
              // paragraph sitting just BEFORE an object — one with a live boundary
              // (`current_boundary.is_some()`) that follows already-emitted content
              // (`!output.is_empty()`), i.e. a durably-recorded empty the user created
              // or an edit produced (e.g. splitting at the end of the paragraph that
              // precedes an image). Excluded: the record-less phantom (current_boundary
              // == None, right after another object) AND the leading sentinel empty
              // before a doc-first object (output still empty — the object stays block 0).
              // Previously an empty `current` was always dropped here, so the full rebuild
              // lost an empty line before an image that the incremental replay kept.
              if !current.runs.is_empty() || (current_boundary.is_some() && !output.is_empty()) {
                push_paragraph_projection_metadata(
                  text,
                  paragraph_index,
                  paragraph_block_index,
                  flow_id,
                  current_boundary,
                  output.len(),
                  last_object_key.as_deref(),
                  paragraph_ids.as_deref_mut(),
                  block_ids.as_deref_mut(),
                  defects,
                );
                output.push(InputBlock::Paragraph(current));
                current = InputParagraph {
                  style: pending_style,
                  runs: Vec::new(),
                };
              }
              output.push(project_object(block, defects)?);
              let object_key = map_string(block, "id")?;
              if let Some(block_ids) = block_ids.as_deref_mut() {
                block_ids.push(BlockId(loro_id_u128(&object_key)));
              }
              last_object_key = Some(object_key);
              current_boundary = None;
            } else if flow_id == ROOT_BODY_FLOW_ID {
              // §5/FS-036 backstop: a placeholder no block claims would
              // silently vanish; report it so the runtime removes the orphan
              // character canonically.
              defects.push(ProjectionDefect::OrphanObjectPlaceholder {
                flow_id: flow_id.to_string(),
                unicode_pos,
              });
            }
          },
          _ => push_char(&mut current, ch, run_styles),
        }
        unicode_pos += 1;
      }
    }

    // A WHOLE flow that ends in object rows still carries `current` — the
    // style-bearing (pending-style) trailing paragraph the editor's document
    // assembly would otherwise mint with a RANDOM id and a default style.
    // Emitting it here keeps the materializer a pure function AND keeps the
    // trailing row's style equal to what the last boundary's mark says (the
    // object-fuzz undo arm caught both: nondeterministic ids, then a stale
    // Normal style after SetParagraphStyle on the interstitial row). Regions
    // and cell flows keep the old behavior — a region legitimately ends
    // mid-geometry and must not gain rows.
    let ends_with_object = flush_trailing_after_object && matches!(output.last(), Some(block) if !matches!(block, InputBlock::Paragraph(_)));
    if ends_with_object && current.runs.is_empty() && current_boundary.is_none() {
      // The synthesized empty trailing row is a FRESH paragraph, not an
      // inheritor: pending style belongs to the last real boundary's
      // paragraph, and carrying it here would make restyling that paragraph
      // silently restyle this row too (maintained-vs-canonical style drift).
      current.style = gpui_flowtext::ParagraphStyle::Normal;
    }
    if !current.runs.is_empty() || current_boundary.is_some() || output.is_empty() && seen_sentinel || ends_with_object {
      push_paragraph_projection_metadata(
        text,
        paragraph_index,
        paragraph_block_index,
        flow_id,
        current_boundary,
        output.len(),
        last_object_key.as_deref(),
        paragraph_ids,
        block_ids,
        defects,
      );
      output.push(InputBlock::Paragraph(current));
    }
    Ok(())
  }

  /// Resolve every object block anchored into `flow_id`. The first return value
  /// maps live placeholder positions to their blocks; the second collects the
  /// quarantined blocks (unresolved or displaced-by-collision anchors) in stable
  /// sorted-key order, with one defect recorded per quarantined block.
  #[allow(clippy::type_complexity, reason = "returns resolved-by-position blocks plus stable-key-ordered quarantined blocks in one pass")]
  #[hotpath::measure]
  fn object_blocks_for_flow(
    &self,
    text: &LoroText,
    flow_id: &str,
    defects: &mut Vec<ProjectionDefect>,
  ) -> io::Result<(BTreeMap<usize, LoroMap>, Vec<(String, LoroMap)>)> {
    // §perf: index the flow snapshot by char position once (O(N)) so the
    // per-object-block anchor check below is O(1); previously each block did
    // `snapshot.chars().nth(pos)` which is O(pos), i.e. O(blocks·len) overall.
    let snapshot_chars: Vec<char> = text.to_string().chars().collect();
    // ONE batched resolver pass for every anchor cursor: a DEAD anchor is
    // simply absent (→ quarantine + canonical re-anchor repair, identity
    // preserved). The former per-record `get_cursor_pos` walked update
    // history for each dead anchor — after an undo of a mass delete restores
    // object records whose anchors all point at deleted placeholders, that
    // was O(objects × history) inside every rematerialization.
    let anchor_positions = boundary_cursor_positions(self.doc, text, &self.blocks, &["anchor_cursor"]);
    let mut by_pos = BTreeMap::new();
    let mut keys_by_pos: BTreeMap<usize, String> = BTreeMap::new();
    let mut quarantined = Vec::new();
    for key in map_keys(&self.blocks) {
      let Some(block) = child_map(&self.blocks, &key)? else {
        continue;
      };
      if map_string_opt(&block, "flow_id")?.as_deref() != Some(flow_id) {
        continue;
      }
      if map_string_opt(&block, "kind")?.as_deref() == Some("paragraph") {
        continue;
      }
      let cursor_bytes = map_binary_opt(&block, "anchor_cursor")?;
      let resolved = cursor_bytes
        .as_deref()
        .and_then(|bytes| Cursor::decode(bytes).ok())
        .filter(|cursor| cursor.container == text.id())
        .and_then(|cursor| match cursor.id {
          Some(id) => anchor_positions.get(&id).copied(),
          None => {
            crate::instrument::record_cursor_pos_resolve();
            self.doc.get_cursor_pos(&cursor).ok().map(|pos| pos.current.pos)
          },
        })
        .filter(|pos| snapshot_chars.get(*pos).copied() == Some(OBJECT_REPLACEMENT));
      let Some(pos) = resolved else {
        // FS-002: never silently drop a block whose anchor is unresolvable.
        defects.push(ProjectionDefect::UnresolvedObjectAnchor {
          block_key: key.clone(),
          flow_id: flow_id.to_string(),
          anchor_cursor: cursor_bytes,
        });
        quarantined.push((key, block));
        continue;
      };
      if let Some(kept_key) = keys_by_pos.get(&pos) {
        // FS-003: colliding cursors previously overwrote each other in the map.
        defects.push(ProjectionDefect::CollidingObjectAnchors {
          flow_id: flow_id.to_string(),
          anchor_unicode: pos,
          kept_block_key: kept_key.clone(),
          displaced_block_key: key.clone(),
        });
        quarantined.push((key, block));
        continue;
      }
      keys_by_pos.insert(pos, key);
      by_pos.insert(pos, block);
    }
    Ok((by_pos, quarantined))
  }

  fn object_block(&self, block: &LoroMap, defects: &mut Vec<ProjectionDefect>) -> io::Result<InputBlock> {
    match map_string(block, "kind")?.as_str() {
      "image" => self.image_block(block, defects).map(InputBlock::Image),
      "equation" => self.equation_block(block).map(InputBlock::Equation),
      "table" => self.table_block(block, defects).map(InputBlock::Table),
      kind => Err(invalid(format!("unsupported Loro block kind `{kind}`"))),
    }
  }

  fn image_block(&self, block: &LoroMap, defects: &mut Vec<ProjectionDefect>) -> io::Result<InputImageBlock> {
    let attrs = child_map(block, "attrs")?;
    // FS-011: an invalid asset id projects as a deterministic placeholder id
    // (never a silent coercion) and is reported for canonical recovery.
    let raw_asset_id = map_string_opt(block, "asset_id")?;
    let asset_id = raw_asset_id.as_deref().and_then(parse_u128);
    if asset_id.is_none() {
      defects.push(ProjectionDefect::InvalidAssetId {
        block_key: map_string_opt(block, "id")?.unwrap_or_default(),
        raw_asset_id,
      });
    }
    Ok(InputImageBlock {
      asset_id: AssetId(asset_id.unwrap_or(0)),
      alt_text: map_string_opt(block, "alt_text_flow_id")?
        .map(|flow_id| self.plain_flow_text(&flow_id))
        .transpose()?
        .unwrap_or_default(),
      caption: map_string_opt(block, "caption_flow_id")?
        .map(|flow_id| self.caption_paragraph(&flow_id))
        .transpose()?,
      sizing: image_sizing(attrs.as_ref())?,
      alignment: alignment(attrs.as_ref())?,
    })
  }

  fn equation_block(&self, block: &LoroMap) -> io::Result<InputEquationBlock> {
    let attrs = child_map(block, "attrs")?;
    Ok(InputEquationBlock {
      source: map_string_opt(block, "source_flow_id")?
        .map(|flow_id| self.plain_flow_text(&flow_id))
        .transpose()?
        .unwrap_or_default(),
      syntax: equation_syntax(attrs.as_ref())?,
      display: equation_display(attrs.as_ref())?,
    })
  }

  fn table_block(&self, owner: &LoroMap, defects: &mut Vec<ProjectionDefect>) -> io::Result<InputTableBlock> {
    let table = child_map(owner, TABLE_KEY)?.ok_or_else(|| invalid("table block has no table map"))?;
    let block_key = map_string_opt(owner, "id")?.unwrap_or_default();
    self.table_from_map(&table, &block_key, defects)
  }

  fn table_from_map(&self, table: &LoroMap, block_key: &str, defects: &mut Vec<ProjectionDefect>) -> io::Result<InputTableBlock> {
    // §28: resolve the table's child containers through their stored raw
    // container ids, falling back to key traversal when unavailable.
    let columns_map = self
      .resolve_child_map(table, "columns_container_id", TABLE_COLUMNS_BY_ID)?
      .ok_or_else(|| invalid("table has no column map"))?;
    let cells_by_id = self
      .resolve_child_map(table, "cells_container_id", TABLE_CELLS_BY_ID)?
      .ok_or_else(|| invalid("table has no cell map"))?;

    // §P2b: read the durable ordered ids, then build the column list carrying
    // each column's durable id + width. Malformed ids (never produced by our
    // writers) are skipped so a single bad id can't sink the whole table.
    let mut columns = Vec::new();
    let mut column_ids = Vec::new();
    for column_id_str in ordered_ids(table, TABLE_COLUMN_ORDER)? {
      let Some(column_id) = parse_column_loro_id(&column_id_str) else {
        continue;
      };
      // A concurrent DeleteTableColumn removes the column's map from `columns_by_id`, but
      // the ordered `column_order` list is a separate CRDT and can still reference it after
      // an out-of-order merge (e.g. concurrent delete + move). Skip the stale order entry
      // rather than failing the whole projection — deterministic across peers (all read the
      // same order + map state), matching the malformed-id skip above and §P2b's "a single
      // bad id can't sink the whole table". Cells left referencing it are dropped by the
      // topology normalization below.
      let Some(column) = child_map(&columns_map, &column_id_str)? else {
        continue;
      };
      columns.push(InputTableColumn {
        id: column_id,
        width: table_column_width(&column)?,
      });
      column_ids.push(column_id);
    }
    let mut row_ids = Vec::new();
    for row_id_str in ordered_ids(table, TABLE_ROW_ORDER)? {
      if let Some(row_id) = parse_row_loro_id(&row_id_str) {
        row_ids.push(row_id);
      }
    }

    // Read every stored cell into a raw record (for topology normalization) and
    // keep its container keyed by coordinate for block projection.
    let mut raw = Vec::new();
    let mut cell_maps: FxHashMap<(u128, u128), LoroMap> = FxHashMap::default();
    for cell_key in cells_by_id.keys().map(|key| key.to_string()) {
      let Some(cell) = child_map(&cells_by_id, &cell_key)? else {
        continue;
      };
      let (Some(row_id), Some(column_id)) = (
        map_string_opt(&cell, "row_id")?.as_deref().and_then(parse_row_loro_id),
        map_string_opt(&cell, "column_id")?.as_deref().and_then(parse_column_loro_id),
      ) else {
        continue;
      };
      let cell_id = parse_cell_loro_id(&cell_key).unwrap_or_else(|| CellId::from_coordinate(row_id, column_id));
      raw.push(table_topology::RawCellRecord {
        row_id,
        column_id,
        cell_id,
        row_span: map_i64_opt(&cell, "row_span")?.and_then(i64_to_u16).unwrap_or(1),
        col_span: map_i64_opt(&cell, "column_span")?.and_then(i64_to_u16).unwrap_or(1),
      });
      cell_maps.insert((row_id.0, column_id.0), cell);
    }

    // §P2b/FS-010: normalize to a full, well-formed grid + a defect list, so
    // every peer reads the identical grid and the runtime repairs the canonical
    // state. `normalize` returns exactly `row_ids.len() * column_ids.len()`
    // cells, row-major.
    let normalized = table_topology::normalize(&row_ids, &column_ids, &raw);
    for defect in &normalized.defects {
      defects.push(map_topology_defect(block_key, defect));
    }

    let column_count = column_ids.len();
    let mut rows = Vec::with_capacity(row_ids.len());
    for (row_index, &row_id) in row_ids.iter().enumerate() {
      let mut cells = Vec::with_capacity(column_count);
      for (col_index, &column_id) in column_ids.iter().enumerate() {
        let normalized_cell = &normalized.cells[row_index * column_count + col_index];
        let blocks = match cell_maps.get(&(row_id.0, column_id.0)) {
          Some(cell_map) if !normalized_cell.synthesized => self.table_cell_blocks(cell_map, defects)?,
          _ => vec![InputTableCellBlock::Paragraph(empty_input_paragraph())],
        };
        cells.push(InputTableCell {
          id: normalized_cell.cell_id,
          row_id,
          column_id,
          blocks,
          row_span: normalized_cell.row_span,
          col_span: normalized_cell.col_span,
        });
      }
      rows.push(InputTableRow { id: row_id, cells });
    }

    Ok(InputTableBlock {
      rows,
      columns,
      style: InputTableStyle {
        header_row: map_bool_opt(table, "header_row")?.unwrap_or(false),
      },
    })
  }

  fn table_cell_blocks(&self, cell: &LoroMap, defects: &mut Vec<ProjectionDefect>) -> io::Result<Vec<InputTableCellBlock>> {
    let flow_id = map_string(cell, "flow_id")?;
    let flow = self.flow_text(&flow_id)?;
    let object_blocks = self.cell_nested_tables(cell, &flow)?;
    let mut projected = Vec::new();
    self.push_flow_blocks(&flow, &object_blocks, &flow_id, None, None, &mut projected, defects, false)?;
    let mut blocks = projected
      .into_iter()
      .filter_map(|block| match block {
        InputBlock::Paragraph(paragraph) => Some(Ok(InputTableCellBlock::Paragraph(paragraph))),
        InputBlock::Table(table) => Some(Ok(InputTableCellBlock::Table(table))),
        InputBlock::Image(_) | InputBlock::Equation(_) => None,
      })
      .collect::<io::Result<Vec<_>>>()?;
    if blocks.is_empty() {
      blocks.push(InputTableCellBlock::Paragraph(empty_input_paragraph()));
    }
    Ok(blocks)
  }

  fn cell_nested_tables(&self, cell: &LoroMap, flow: &LoroText) -> io::Result<BTreeMap<usize, LoroMap>> {
    let mut tables = BTreeMap::new();
    let Some(tables_by_id) = child_map(cell, "nested_tables_by_id")? else {
      return Ok(tables);
    };
    for nested_table_id in ordered_ids(cell, "nested_table_ids")? {
      let Some(owner) = child_map(&tables_by_id, &nested_table_id)? else {
        continue;
      };
      let Some(cursor_bytes) = map_binary_opt(&owner, "anchor_cursor")? else {
        continue;
      };
      let Ok(cursor) = Cursor::decode(&cursor_bytes) else {
        continue;
      };
      if cursor.container != flow.id() {
        continue;
      }
      crate::instrument::record_cursor_pos_resolve();
      if let Ok(pos) = self.doc.get_cursor_pos(&cursor) {
        tables.insert(pos.current.pos, owner);
      }
    }
    Ok(tables)
  }

  fn flow_text(&self, flow_id: &str) -> io::Result<LoroText> {
    let flow = child_map(&self.flows, flow_id)?.ok_or_else(|| invalid(format!("missing flow `{flow_id}`")))?;
    // §28: prefer direct resolution via the flow's stored raw container id, and
    // only fall back to map-key traversal when the id is missing/unresolvable.
    if let Some(container_id) = map_string_opt(&flow, "text_container_id")?
      && let Some(text) = resolve_text_by_container_id(self.doc, &container_id)
    {
      return Ok(text);
    }
    child_text(&flow, FLOW_TEXT_KEY)?.ok_or_else(|| invalid(format!("flow `{flow_id}` has no text")))
  }

  /// §28: resolve a child container map by its stored raw container id, falling
  /// back to direct map-key traversal when the id is missing/unresolvable.
  fn resolve_child_map(&self, owner: &LoroMap, container_id_key: &str, fallback_key: &str) -> io::Result<Option<LoroMap>> {
    if let Some(container_id) = map_string_opt(owner, container_id_key)?
      && let Some(map) = resolve_map_by_container_id(self.doc, &container_id)
    {
      return Ok(Some(map));
    }
    child_map(owner, fallback_key)
  }

  fn plain_flow_text(&self, flow_id: &str) -> io::Result<String> {
    // §perf: drop a single leading '\n' in place rather than allocating a second
    // String via strip_prefix(..).to_string().
    let mut text = self.flow_text(flow_id)?.to_string();
    if text.starts_with('\n') {
      text.remove(0);
    }
    Ok(text)
  }

  fn caption_paragraph(&self, flow_id: &str) -> io::Result<InputParagraph> {
    let paragraphs = paragraphs_from_text(&self.flow_text(flow_id)?);
    Ok(paragraphs.into_iter().next().unwrap_or(InputParagraph {
      style: gpui_flowtext::ParagraphStyle::Normal,
      runs: Vec::new(),
    }))
  }
}

/// §5 fidelity integrity invariant for the body projection. The caller gates this
/// on [`fidelity::enabled`]; it never mutates the document and only reads the
/// already-resolved body text and object-block map. It asserts the canonical
/// invariants that the projector's defect reporting is designed to guarantee:
/// * `missing-sentinel` (Structure): the body flow starts with the sentinel newline.
/// * `boundary-without-mark` (Structure): every boundary newline carries a
///   paragraph-style mark.
/// * `orphan-object` (Structure): every U+FFFC placeholder resolves to a live
///   block record (a key of `object_blocks`).
/// * `orphan-metadata` (Identity): no non-paragraph block record dangles without a
///   live U+FFFC placeholder (surfaced as an unresolved-anchor defect).
///
/// (d) A fabricated-id paragraph is only ever emitted alongside a recorded
/// `MissingParagraph*` defect (see [`push_paragraph_projection_metadata`] and the
/// empty-projection placeholder), each of which is surfaced by the per-defect
/// `Structure`/`defect` events, so fabricated identities need no separate scan.
fn check_body_projection_integrity(body: &LoroText, object_blocks: &BTreeMap<usize, LoroMap>, defects: &[ProjectionDefect]) {
  let snapshot = body.to_string();
  fidelity::check(
    snapshot.starts_with(crate::SENTINEL_NEWLINE),
    FidelityClass::Structure,
    "missing-sentinel",
    || format!("body flow does not start with the sentinel newline (first char {:?})", snapshot.chars().next()),
  );
  // (b) Every boundary newline must carry a paragraph-style mark. Walk the rich
  // delta so we see each insert's attributes, mirroring the projector's own scan.
  let mut unicode_pos = 0_usize;
  for item in body.to_delta() {
    let loro::TextDelta::Insert { insert, attributes } = item else {
      continue;
    };
    for ch in insert.chars() {
      if ch == '\n' {
        fidelity::check(
          paragraph_style_from_attrs(attributes.as_ref()).is_some(),
          FidelityClass::Structure,
          "boundary-without-mark",
          || format!("paragraph boundary newline at body unicode pos {unicode_pos} carries no paragraph-style mark"),
        );
      }
      unicode_pos += 1;
    }
  }
  // (c) Every live U+FFFC placeholder must be claimed by a resolved block record.
  for (pos, ch) in snapshot.chars().enumerate() {
    if ch == OBJECT_REPLACEMENT {
      fidelity::check(
        object_blocks.contains_key(&pos),
        FidelityClass::Structure,
        "orphan-object",
        || format!("U+FFFC object placeholder at body unicode pos {pos} has no live block record"),
      );
    }
  }
  // (c, vice-versa) A block record whose anchor no longer resolves to a live
  // placeholder is dangling metadata; the projector reports it as an
  // unresolved-anchor defect, which this invariant escalates loudly.
  for defect in defects {
    if let ProjectionDefect::UnresolvedObjectAnchor { block_key, flow_id, .. } = defect {
      fidelity::check(
        false,
        FidelityClass::Identity,
        "orphan-metadata",
        || format!("block `{block_key}` in flow `{flow_id}` has no live U+FFFC placeholder"),
      );
    }
  }
}

fn paragraphs_from_text(text: &LoroText) -> Vec<InputParagraph> {
  let mut blocks = Vec::new();
  let projector = ParagraphOnlyProjector;
  projector.push_flow_blocks(text, &mut blocks);
  blocks
}

struct ParagraphOnlyProjector;

impl ParagraphOnlyProjector {
  fn push_flow_blocks(&self, text: &LoroText, output: &mut Vec<InputParagraph>) {
    let mut current = InputParagraph {
      style: gpui_flowtext::ParagraphStyle::Normal,
      runs: Vec::new(),
    };
    let mut pending_style = gpui_flowtext::ParagraphStyle::Normal;
    let mut seen_sentinel = false;
    for item in text.to_delta() {
      let loro::TextDelta::Insert { insert, attributes } = item else {
        continue;
      };
      let run_styles = run_styles_from_attrs(attributes.as_ref());
      for ch in insert.chars() {
        if ch == '\n' {
          let style = paragraph_style_from_attrs(attributes.as_ref()).unwrap_or(pending_style);
          if !seen_sentinel {
            seen_sentinel = true;
            pending_style = style;
            current.style = style;
          } else {
            output.push(current);
            current = InputParagraph { style, runs: Vec::new() };
            pending_style = style;
          }
        } else if ch != OBJECT_REPLACEMENT {
          push_char(&mut current, ch, run_styles);
        }
      }
    }
    if seen_sentinel || !current.runs.is_empty() {
      output.push(current);
    }
  }
}

fn push_char(paragraph: &mut InputParagraph, ch: char, styles: RunStyles) {
  if let Some(last) = paragraph.runs.last_mut()
    && last.styles == styles
  {
    last.text.push(ch);
    return;
  }
  paragraph.runs.push(InputRun {
    text: ch.to_string(),
    styles,
  });
}

#[allow(clippy::too_many_arguments, reason = "paragraph metadata projection needs the flow text, prebuilt boundary indexes, flow/boundary context, id pools and defect sink")]
fn push_paragraph_projection_metadata(
  text: &LoroText,
  paragraph_index: Option<&FxHashMap<usize, String>>,
  paragraph_block_index: Option<&FxHashMap<usize, String>>,
  flow_id: &str,
  boundary: Option<usize>,
  block_ix: usize,
  interstitial_anchor: Option<&str>,
  paragraph_ids: Option<&mut Vec<ParagraphId>>,
  block_ids: Option<&mut Vec<BlockId>>,
  defects: &mut Vec<ProjectionDefect>,
) {
  let paragraph_resolved = boundary.and_then(|boundary| paragraph_index.and_then(|index| index.get(&boundary)).cloned());
  let block_resolved = boundary.and_then(|boundary| paragraph_block_index.and_then(|index| index.get(&boundary)).cloned());
  // §5: fabricate stable, position-independent ids (from the boundary's OpID —
  // the SAME keys the repair writer mints) for any boundary without a durable
  // record. Resolve the anchor once, and only when we actually must fabricate,
  // since `get_cursor` is not free.
  let needs_fabrication = (paragraph_ids.is_some() && paragraph_resolved.is_none()) || (block_ids.is_some() && block_resolved.is_none());
  let fabricated_keys = needs_fabrication.then(|| boundary.and_then(|boundary| stable_boundary_metadata_keys(text, boundary))).flatten();

  if let Some(paragraph_ids) = paragraph_ids {
    // §5/FS-004: a boundary with no durable paragraph metadata gets a fabricated,
    // projection-only id (reported so the runtime writes the real record). Deriving
    // it from the boundary OpID makes it identical to the repaired record's id and
    // stable across peers, so incremental/full and both peers converge.
    let id = match paragraph_resolved {
      Some(id) => id,
      None => {
        // Boundary-less (interstitial/trailing) rows anchor their fabricated
        // identity to the durable key of the OBJECT they follow — a row-index
        // key ("paragraph.projection.{ix}") silently re-identified the row on
        // every edit above it (the object-fuzz undo arm caught the drift as a
        // maintained-vs-canonical id mismatch after a join).
        let fabricated_id = fabricated_keys.as_ref().map(|(paragraph_key, _)| paragraph_key.clone()).unwrap_or_else(|| {
          interstitial_anchor.map_or_else(|| format!("paragraph.projection.{block_ix}"), |anchor| format!("paragraph.after.{anchor}"))
        });
        defects.push(ProjectionDefect::MissingParagraphMetadata {
          flow_id: flow_id.to_string(),
          boundary_unicode: boundary,
          fabricated_id: loro_id_u128(&fabricated_id),
        });
        fabricated_id
      },
    };
    paragraph_ids.push(ParagraphId(loro_id_u128(&id)));
  }
  if let Some(block_ids) = block_ids {
    // §5/FS-005: mirror the paragraph-metadata report for the paragraph block registry.
    let id = match block_resolved {
      Some(id) => id,
      None => {
        let fabricated_id = fabricated_keys.as_ref().map(|(_, block_key)| block_key.clone()).unwrap_or_else(|| {
          interstitial_anchor
            .map_or_else(|| format!("paragraph_block.projection.{block_ix}"), |anchor| format!("paragraph_block.after.{anchor}"))
        });
        defects.push(ProjectionDefect::MissingParagraphBlock {
          flow_id: flow_id.to_string(),
          boundary_unicode: boundary,
          fabricated_id: loro_id_u128(&fabricated_id),
        });
        fabricated_id
      },
    };
    block_ids.push(BlockId(loro_id_u128(&id)));
  }
}

/// Build a `boundary position → paragraph metadata loro id` index for `text` in a
/// SINGLE pass over the paragraph registry. Projecting a flow calls this once and
/// then does O(1) lookups per boundary — replacing the former
/// `paragraph_loro_id_at_boundary` which rescanned every paragraph record for each
/// boundary, an O(paragraphs²·chars) hot path that hung the CRDT actor thread when
/// materializing a large document.
///
/// Selection matches the previous scan exactly: keys are visited in sorted order
/// (via [`map_keys`]) so the lexicographically-smallest id wins a shared boundary,
/// except boundary 0 always prefers `ROOT_FIRST_PARAGRAPH_ID` when it anchors there.
#[hotpath::measure]
fn paragraph_ids_by_boundary(doc: &LoroDoc, text: &LoroText) -> FxHashMap<usize, String> {
  let mut index: FxHashMap<usize, String> = FxHashMap::default();
  let root = doc.get_map(ROOT);
  let Some(paragraphs) = child_map(&root, PARAGRAPHS_BY_ID).ok().flatten() else {
    return index;
  };
  let pos_by_id = boundary_cursor_positions(doc, text, &paragraphs, &["boundary_cursor", "start_cursor"]);
  let mut root_first_at_zero = false;
  for key in map_keys(&paragraphs) {
    let Some(paragraph) = child_map(&paragraphs, &key).ok().flatten() else {
      continue;
    };
    let Some(pos) = live_cursor_pos(doc, text, &paragraph, "boundary_cursor", &pos_by_id)
      .or_else(|| live_cursor_pos(doc, text, &paragraph, "start_cursor", &pos_by_id))
    else {
      continue;
    };
    if pos == 0 && key.as_str() == ROOT_FIRST_PARAGRAPH_ID {
      root_first_at_zero = true;
    }
    index.entry(pos).or_insert(key);
  }
  if root_first_at_zero {
    index.insert(0, ROOT_FIRST_PARAGRAPH_ID.to_string());
  }
  index
}

/// Build a `boundary position → paragraph *block* loro id` index for `text` in a
/// single pass over the block registry (paragraph-kind blocks only). Companion to
/// [`paragraph_ids_by_boundary`] with the same one-pass rationale and selection
/// rule, except boundary 0 prefers `MAIN_BODY_BLOCK_ID`.
#[hotpath::measure]
fn paragraph_block_ids_by_boundary(doc: &LoroDoc, text: &LoroText) -> FxHashMap<usize, String> {
  let mut index: FxHashMap<usize, String> = FxHashMap::default();
  let root = doc.get_map(ROOT);
  let Some(blocks) = child_map(&root, BLOCKS_BY_ID).ok().flatten() else {
    return index;
  };
  let pos_by_id = boundary_cursor_positions(doc, text, &blocks, &["anchor_cursor"]);
  let mut main_body_at_zero = false;
  for key in map_keys(&blocks) {
    let Some(block) = child_map(&blocks, &key).ok().flatten() else {
      continue;
    };
    if map_string_opt(&block, "kind").ok().flatten().as_deref() != Some("paragraph") {
      continue;
    }
    let Some(pos) = live_cursor_pos(doc, text, &block, "anchor_cursor", &pos_by_id) else {
      continue;
    };
    if pos == 0 && key.as_str() == MAIN_BODY_BLOCK_ID {
      main_body_at_zero = true;
    }
    index.entry(pos).or_insert(key);
  }
  if main_body_at_zero {
    index.insert(0, MAIN_BODY_BLOCK_ID.to_string());
  }
  index
}

/// Resolve, in a SINGLE pass over `text`, the current position of every live
/// boundary cursor stored under `cursor_fields` across `records`, returning an
/// `id → position` map. Each record contributes O(1) cursor decodes here, and the
/// whole set of positions is resolved by one `query_text_id_positions` chunk scan
/// (vendored Loro batch resolver) instead of an O(elements) `get_cursor_pos` per
/// record. That is what takes `document_from_loro` from O(records²) — which pegged
/// the CRDT actor at 100% CPU on a large document — down to ~O(elements). Ids not
/// present (deleted) are simply absent; a DEAD anchor is treated as unresolvable
/// (fabrication/quarantine + canonical repair), never history-traced per cursor.
#[hotpath::measure]
fn boundary_cursor_positions(doc: &LoroDoc, text: &LoroText, records: &LoroMap, cursor_fields: &[&str]) -> FxHashMap<ID, usize> {
  let container = text.id();
  let mut ids: Vec<ID> = Vec::new();
  // §perf: the result is an ID-keyed map that never depends on iteration order, so
  // walk the block keys directly (InternalString derefs to &str) rather than
  // collecting a sorted Vec<String> of every key just to discard the ordering.
  for key in records.keys() {
    let Some(record) = child_map(records, &key).ok().flatten() else {
      continue;
    };
    for field in cursor_fields {
      if let Some(bytes) = map_binary_opt(&record, field).ok().flatten()
        && let Ok(cursor) = Cursor::decode(&bytes)
        && cursor.container == container
        && let Some(id) = cursor.id
      {
        ids.push(id);
      }
    }
  }
  let mut positions = FxHashMap::default();
  if ids.is_empty() {
    return positions;
  }
  for (id, pos) in ids.iter().copied().zip(doc.inner().query_text_id_positions(&container, &ids)) {
    if let Some(pos) = pos {
      positions.insert(id, pos);
    }
  }
  positions
}

fn live_cursor_pos(doc: &LoroDoc, text: &LoroText, map: &LoroMap, key: &str, pos_by_id: &FxHashMap<ID, usize>) -> Option<usize> {
  let cursor_bytes = map_binary_opt(map, key).ok().flatten()?;
  let cursor = Cursor::decode(&cursor_bytes).ok()?;
  if cursor.container != text.id() {
    return None;
  }
  // The boundary index resolved every id-carrying live cursor in one batch
  // pass, so this is an O(1) lookup — and absence means the anchor is DELETED.
  // Falling back to per-id `get_cursor_pos` for dead anchors walks update
  // history per cursor: after an undo of a mass delete restores records whose
  // cursors all point at the deleted characters, that was O(records × history)
  // — a multi-minute rematerialization freeze (the ctrl-A + undo field bug).
  // A dead anchor resolves to None; the boundary fabricates its deterministic
  // id and canonical defect repair re-anchors the record — identical recovery
  // law on every peer. Id-less cursors (F7) keep the per-cursor resolution.
  let pos = match cursor.id {
    Some(id) => pos_by_id.get(&id).copied()?,
    None => {
      crate::instrument::record_cursor_pos_resolve();
      doc.get_cursor_pos(&cursor).ok()?.current.pos
    },
  };
  // `pos` is a live Unicode-code-point index into `text`; validate it is in range
  // with an O(1) length check (never materialize the flow string).
  (pos < text.len_unicode()).then_some(pos)
}

fn map_keys(map: &LoroMap) -> Vec<String> {
  let mut keys = map.keys().map(|key| key.to_string()).collect::<Vec<_>>();
  keys.sort();
  keys
}

fn loro_id_u128(id: &str) -> u128 {
  if let Some(value) = id
    .rsplit('.')
    .next()
    .and_then(|suffix| suffix.parse::<u128>().ok())
  {
    return value;
  }
  let hash = blake3::hash(id.as_bytes());
  let mut bytes = [0_u8; 16];
  bytes.copy_from_slice(&hash.as_bytes()[..16]);
  u128::from_le_bytes(bytes)
}

/// Deterministic `(paragraph_key, block_key)` for the durable metadata records that
/// anchor `boundary` in `text` — the SINGLE source of these ids, shared by the
/// projection (which fabricates them when a boundary has no durable record) and the
/// runtime's repair writer (which materializes the records). Because both derive
/// the same key, a fabricated id and a later-repaired record's id are the SAME
/// value on every peer, so they converge instead of clobbering each other.
///
/// The keys are POSITION-INDEPENDENT: boundary 0 is the canonical first paragraph;
/// every other boundary derives from the boundary newline's stable Loro `OpID`
/// (globally unique and insertion-stable), so unlike a `block_ix` or unicode-offset
/// key it does not change when text shifts and is identical across peers. The
/// non-numeric `op-…` suffix routes both keys through `loro_id_u128`'s hash (rather
/// than its trailing-number rule), keeping the paragraph and block ids distinct.
/// Returns `None` only when `boundary` has no live anchor (e.g. an empty container).
/// Materialize ONE table block from canonical state — the same law
/// `document_from_loro` applies to it, exposed so table-op patch synthesis
/// READS the committed table back instead of simulating the op on the old
/// projection. Simulation is a second doc→projection semantics and diverged
/// from canonical under undo-churned histories (found by the table-fuzz undo
/// arm as stale cell spans). A missing canonical record or any reported
/// defect is the caller's cue to fall back to the full rebuild.
pub fn materialize_table_block(doc: &LoroDoc, block_id: u128) -> io::Result<(InputTableBlock, Vec<ProjectionDefect>)> {
  let projector = Projector::new(doc)?;
  let record = child_map(&projector.blocks, &format!("table.{block_id}"))?.ok_or_else(|| invalid("table block record missing"))?;
  let mut defects = Vec::new();
  let table = projector.table_block(&record, &mut defects)?;
  Ok((table, defects))
}

#[must_use]
pub fn stable_boundary_metadata_keys(text: &LoroText, boundary: usize) -> Option<(String, String)> {
  if boundary == 0 {
    return Some((ROOT_FIRST_PARAGRAPH_ID.to_string(), MAIN_BODY_BLOCK_ID.to_string()));
  }
  let anchor = text.get_cursor(boundary, Side::Left)?.id?;
  Some((
    format!("paragraph.anchor.op-{}-{}", anchor.peer, anchor.counter),
    format!("paragraph_block.anchor.op-{}-{}", anchor.peer, anchor.counter),
  ))
}

fn child_map(parent: &LoroMap, key: &str) -> io::Result<Option<LoroMap>> {
  Ok(parent.get(key).and_then(|value| match value {
    ValueOrContainer::Container(container) => container.into_map().ok(),
    ValueOrContainer::Value(_) => None,
  }))
}

fn child_text(parent: &LoroMap, key: &str) -> io::Result<Option<LoroText>> {
  Ok(parent.get(key).and_then(|value| match value {
    ValueOrContainer::Container(container) => container.into_text().ok(),
    ValueOrContainer::Value(_) => None,
  }))
}

/// §28: centralized resolution of a stored raw Loro container id string.
///
/// Parses the durable `*_container_id` string into a [`ContainerID`] and fetches
/// the live container directly from the document for efficient runtime access.
/// Returns `None` when the id is missing/unparseable or the container is
/// absent/detached/deleted, so callers can fall back to map-key traversal.
fn resolve_container(doc: &LoroDoc, container_id: &str) -> Option<Container> {
  let container = doc.get_container(ContainerID::try_from(container_id).ok()?)?;
  (container.is_attached() && !container.is_deleted()).then_some(container)
}

fn resolve_map_by_container_id(doc: &LoroDoc, container_id: &str) -> Option<LoroMap> {
  resolve_container(doc, container_id)?.into_map().ok()
}

fn resolve_text_by_container_id(doc: &LoroDoc, container_id: &str) -> Option<LoroText> {
  resolve_container(doc, container_id)?.into_text().ok()
}

fn ordered_ids(map: &LoroMap, key: &str) -> io::Result<Vec<String>> {
  let Some(ValueOrContainer::Container(container)) = map.get(key) else {
    return Ok(Vec::new());
  };
  let value = match container {
    Container::MovableList(list) => list.get_deep_value(),
    _ => return Ok(Vec::new()),
  };
  Ok(
    value
      .into_list()
      .unwrap_or_default()
      .iter()
      .filter_map(|value| match value {
        LoroValue::String(value) => Some(value.to_string()),
        _ => None,
      })
      .collect(),
  )
}

fn map_string(map: &LoroMap, key: &str) -> io::Result<String> {
  map_string_opt(map, key)?.ok_or_else(|| invalid(format!("missing string field `{key}`")))
}

fn map_string_opt(map: &LoroMap, key: &str) -> io::Result<Option<String>> {
  Ok(map.get(key).and_then(|value| match value {
    ValueOrContainer::Value(LoroValue::String(value)) => Some(value.to_string()),
    _ => None,
  }))
}

fn map_binary_opt(map: &LoroMap, key: &str) -> io::Result<Option<Vec<u8>>> {
  Ok(map.get(key).and_then(|value| match value {
    ValueOrContainer::Value(LoroValue::Binary(value)) => Some(value.to_vec()),
    _ => None,
  }))
}

fn map_i64_opt(map: &LoroMap, key: &str) -> io::Result<Option<i64>> {
  Ok(map.get(key).and_then(|value| match value {
    ValueOrContainer::Value(LoroValue::I64(value)) => Some(value),
    _ => None,
  }))
}

fn map_bool_opt(map: &LoroMap, key: &str) -> io::Result<Option<bool>> {
  Ok(map.get(key).and_then(|value| match value {
    ValueOrContainer::Value(LoroValue::Bool(value)) => Some(value),
    _ => None,
  }))
}

fn paragraph_style_from_attrs(attrs: Option<&FxHashMap<String, LoroValue>>) -> Option<gpui_flowtext::ParagraphStyle> {
  let value = attrs?.get(MARK_PARAGRAPH_STYLE)?;
  match value {
    LoroValue::I64(0) => Some(gpui_flowtext::ParagraphStyle::Normal),
    LoroValue::I64(slot) if *slot > 0 => u8::try_from(*slot - 1)
      .ok()
      .map(gpui_flowtext::ParagraphStyle::Custom),
    _ => None,
  }
}

fn run_styles_from_attrs(attrs: Option<&FxHashMap<String, LoroValue>>) -> RunStyles {
  let mut styles = RunStyles::default();
  let Some(attrs) = attrs else {
    return styles;
  };
  if let Some(LoroValue::I64(slot)) = attrs.get(MARK_RUN_SEMANTIC_STYLE)
    && let Ok(slot) = u8::try_from(*slot)
  {
    styles.semantic = RunSemanticStyle::Custom(slot);
  }
  if let Some(LoroValue::I64(slot)) = attrs.get(MARK_HIGHLIGHT_STYLE)
    && let Ok(slot) = u8::try_from(*slot)
  {
    styles.highlight = Some(HighlightStyle::Custom(slot));
  }
  if matches!(attrs.get(MARK_DIRECT_UNDERLINE), Some(LoroValue::Bool(true))) {
    styles.direct_underline = true;
  }
  if matches!(attrs.get(MARK_STRIKETHROUGH), Some(LoroValue::Bool(true))) {
    styles.strikethrough = true;
  }
  styles
}

fn image_sizing(attrs: Option<&LoroMap>) -> io::Result<InputImageSizing> {
  let Some(attrs) = attrs else {
    return Ok(InputImageSizing::FitWidth);
  };
  match map_string_opt(attrs, "sizing")?.as_deref() {
    Some("intrinsic") => Ok(InputImageSizing::Intrinsic),
    Some("fixed") => Ok(InputImageSizing::Fixed {
      width_px: map_i64_opt(attrs, "width_px")?
        .and_then(i64_to_u32)
        .unwrap_or(640),
      height_px: map_i64_opt(attrs, "height_px")?.and_then(i64_to_u32),
    }),
    Some("fit_width") | None => Ok(InputImageSizing::FitWidth),
    Some(_) => Ok(InputImageSizing::FitWidth),
  }
}

fn alignment(attrs: Option<&LoroMap>) -> io::Result<InputBlockAlignment> {
  let Some(attrs) = attrs else {
    return Ok(InputBlockAlignment::Left);
  };
  Ok(match map_string_opt(attrs, "alignment")?.as_deref() {
    Some("center") => InputBlockAlignment::Center,
    Some("right") => InputBlockAlignment::Right,
    Some("left") | None => InputBlockAlignment::Left,
    Some(_) => InputBlockAlignment::Left,
  })
}

fn equation_syntax(attrs: Option<&LoroMap>) -> io::Result<InputEquationSyntax> {
  let Some(attrs) = attrs else {
    return Ok(InputEquationSyntax::Latex);
  };
  Ok(match map_string_opt(attrs, "syntax")?.as_deref() {
    Some("latex") | None => InputEquationSyntax::Latex,
    Some(_) => InputEquationSyntax::Latex,
  })
}

fn equation_display(attrs: Option<&LoroMap>) -> io::Result<InputEquationDisplay> {
  let Some(attrs) = attrs else {
    return Ok(InputEquationDisplay::Display);
  };
  Ok(match map_string_opt(attrs, "display")?.as_deref() {
    Some("inline_like_paragraph") => InputEquationDisplay::InlineLikeParagraph,
    Some("display") | None => InputEquationDisplay::Display,
    Some(_) => InputEquationDisplay::Display,
  })
}

fn table_column_width(column: &LoroMap) -> io::Result<InputTableColumnWidth> {
  Ok(match map_string_opt(column, "width_kind")?.as_deref() {
    Some("fixed_px") => InputTableColumnWidth::FixedPx(
      map_i64_opt(column, "width_px")?
        .and_then(i64_to_u32)
        .unwrap_or(120),
    ),
    Some("fraction") => InputTableColumnWidth::Fraction(
      map_i64_opt(column, "fraction")?
        .and_then(i64_to_u32)
        .unwrap_or(1),
    ),
    Some("auto") | None => InputTableColumnWidth::Auto,
    Some(_) => InputTableColumnWidth::Auto,
  })
}

/// Map a pure [`table_topology`] grid defect onto a [`ProjectionDefect`] the
/// runtime repair pipeline understands (§P2b / FS-010).
fn map_topology_defect(block_key: &str, defect: &table_topology::TableTopologyDefect) -> ProjectionDefect {
  use table_topology::TableTopologyDefect as Defect;
  let (row_id, column_id, kind) = match defect {
    Defect::MissingCell { row_id, column_id } => (row_id.0, column_id.0, TableTopologyKind::MissingCell),
    Defect::DuplicateCoordinate { row_id, column_id } => (row_id.0, column_id.0, TableTopologyKind::DuplicateCoordinate),
    Defect::InvalidSpan { row_id, column_id } => (row_id.0, column_id.0, TableTopologyKind::InvalidSpan),
    Defect::OrphanCell { row_id, column_id } => (row_id.0, column_id.0, TableTopologyKind::OrphanCell),
  };
  ProjectionDefect::TableTopology {
    table_block_key: block_key.to_string(),
    row_id: Some(row_id),
    column_id: Some(column_id),
    kind,
  }
}

/// The deterministic empty cell/quarantine paragraph the projector emits for a
/// synthesized or empty table cell.
fn empty_input_paragraph() -> InputParagraph {
  InputParagraph {
    style: gpui_flowtext::ParagraphStyle::Normal,
    runs: Vec::new(),
  }
}

fn parse_u128(value: &str) -> Option<u128> {
  value.parse::<u128>().ok()
}

fn i64_to_u32(value: i64) -> Option<u32> {
  u32::try_from(value).ok()
}

fn i64_to_u16(value: i64) -> Option<u16> {
  u16::try_from(value).ok()
}

fn i64_to_u8(value: i64) -> Option<u8> {
  u8::try_from(value).ok()
}

fn section_id_field(map: &LoroMap, key: &str) -> io::Result<Option<u128>> {
  Ok(map_string_opt(map, key)?.and_then(|value| parse_u128(&value)))
}

/// §11: read a section's page-structure attributes back from the canonical Loro
/// document, substituting documented defaults for any missing keys. Returns
/// `None` only when the named section does not exist.
///
/// `DocumentProjection` now carries these attrs on `DocumentSection::page`,
/// populated from canonical Loro during projection (see
/// [`Projector::sections_for_projection`]). This helper remains the direct,
/// single-section read-back path for callers that only need one section's attrs
/// without projecting the whole document. The canonical values always live in
/// Loro and round-trip losslessly.
#[must_use]
pub fn section_page_attrs(doc: &LoroDoc, section_id: &str) -> Option<crate::loro_schema::SectionPageAttrs> {
  let root = doc.get_map(ROOT);
  let sections = child_map(&root, SECTIONS_BY_ID).ok().flatten()?;
  let section = child_map(&sections, section_id).ok().flatten()?;
  let attrs = child_map(&section, "attrs").ok().flatten()?;
  Some(crate::loro_schema::read_section_page_attrs(&attrs))
}

/// §11: map canonical Loro page-structure attrs
/// (`crate::loro_schema::SectionPageAttrs`) onto the gpui-flowtext projection
/// mirror (`gpui_flowtext::SectionPageAttrs`). gpui-flowtext cannot depend on
/// `flowstate-document`, so the two types are defined field-for-field
/// identically and this is a direct copy. Fully-qualified paths disambiguate the
/// clashing type names. Takes the canonical attrs by value so the owned
/// header/footer flow id strings move rather than clone.
fn project_section_page_attrs(attrs: crate::loro_schema::SectionPageAttrs) -> gpui_flowtext::SectionPageAttrs {
  gpui_flowtext::SectionPageAttrs {
    page_size: gpui_flowtext::SectionPageSize {
      width_twips: attrs.page_size.width_twips,
      height_twips: attrs.page_size.height_twips,
    },
    margins: gpui_flowtext::SectionMargins {
      top_twips: attrs.margins.top_twips,
      right_twips: attrs.margins.right_twips,
      bottom_twips: attrs.margins.bottom_twips,
      left_twips: attrs.margins.left_twips,
    },
    columns: attrs.columns,
    orientation: match attrs.orientation {
      crate::loro_schema::SectionOrientation::Portrait => gpui_flowtext::SectionOrientation::Portrait,
      crate::loro_schema::SectionOrientation::Landscape => gpui_flowtext::SectionOrientation::Landscape,
    },
    page_numbering: gpui_flowtext::SectionPageNumbering {
      format: match attrs.page_numbering.format {
        crate::loro_schema::PageNumberFormat::None => gpui_flowtext::PageNumberFormat::None,
        crate::loro_schema::PageNumberFormat::Decimal => gpui_flowtext::PageNumberFormat::Decimal,
        crate::loro_schema::PageNumberFormat::LowerRoman => gpui_flowtext::PageNumberFormat::LowerRoman,
        crate::loro_schema::PageNumberFormat::UpperRoman => gpui_flowtext::PageNumberFormat::UpperRoman,
        crate::loro_schema::PageNumberFormat::LowerAlpha => gpui_flowtext::PageNumberFormat::LowerAlpha,
        crate::loro_schema::PageNumberFormat::UpperAlpha => gpui_flowtext::PageNumberFormat::UpperAlpha,
      },
      start: attrs.page_numbering.start,
    },
    header_flow_id: attrs.header_flow_id,
    footer_flow_id: attrs.footer_flow_id,
  }
}

fn invalid(message: impl Into<String>) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{document_to_loro, flowstate_document_theme, loro_schema::body_text};
  use gpui_flowtext::{
    InputBlock, InputBlockAlignment, InputImageBlock, InputImageSizing, InputParagraph, InputRun, RunStyles, document_from_input_blocks,
  };

  /// §act-four M4 cold viewport load: `materialize_viewport` materializes an
  /// arbitrary body region byte-identically to the corresponding slice of the
  /// full `document_from_loro` rebuild — WITHOUT building the whole projection.
  #[test]
  fn materialize_viewport_matches_the_full_rebuild_slice() -> io::Result<()> {
    let source = document_from_input_blocks(
      DocumentTheme::clone(&flowstate_document_theme()),
      (0..12)
        .map(|ix| {
          InputBlock::Paragraph(InputParagraph {
            style: gpui_flowtext::ParagraphStyle::Normal,
            runs: vec![InputRun {
              text: format!("paragraph {ix} — naïve café ☃"),
              styles: RunStyles::default(),
            }],
          })
        })
        .collect(),
    );
    let doc = document_to_loro(&source, "Viewport load")?;
    let body = body_text(&doc);
    let full = projection_blocks_from_loro(&doc)?.blocks; // full rebuild, Vec<InputBlock>

    // (1) The whole-doc viewport equals the full rebuild, block-for-block.
    let whole = materialize_viewport(&doc, 0, body.len_unicode())?;
    assert_eq!(whole.blocks, full, "whole-doc viewport == full rebuild");

    // (2) A mid-doc sub-viewport equals the corresponding slice of the full
    // rebuild — the cold random-scroll case. Boundaries (sorted by position)
    // index paragraphs; [boundary[6], boundary[9]) covers paragraphs 6,7,8.
    let mut boundaries: Vec<usize> = paragraph_ids_by_boundary(&doc, &body).keys().copied().collect();
    boundaries.sort_unstable();
    assert!(boundaries.len() >= 10, "enough rows to sub-viewport");
    let viewport = materialize_viewport(&doc, boundaries[6], boundaries[9])?;
    assert_eq!(viewport.blocks, full[6..9].to_vec(), "sub-viewport == full[6..9], byte-identical");
    Ok(())
  }

  /// The vendored Loro batch resolver (`query_text_id_positions`, driving
  /// `boundary_cursor_positions`) must produce EXACTLY the positions the per-id
  /// `get_cursor_pos` produces — that equivalence is the entire correctness basis
  /// for replacing the O(records²) per-cursor scan. Resolves every boundary cursor
  /// in one batch call (exercising the by-peer grouping + binary search) and checks
  /// each against `get_cursor_pos`; multibyte content is included so any
  /// unicode/event index mismatch would surface.
  #[test]
  fn batch_cursor_resolver_matches_per_cursor_get_cursor_pos() -> io::Result<()> {
    let source = document_from_input_blocks(
      DocumentTheme::clone(&flowstate_document_theme()),
      (0..40)
        .map(|ix| {
          InputBlock::Paragraph(InputParagraph {
            style: gpui_flowtext::ParagraphStyle::Normal,
            runs: vec![InputRun { text: format!("paragraph {ix} — naïve café ☃ tail"), styles: RunStyles::default() }],
          })
        })
        .collect(),
    );
    let doc = document_to_loro(&source, "Batch cursor equivalence")?;
    let root = doc.get_map(ROOT);
    let body = body_text(&doc);
    let paragraphs = child_map(&root, PARAGRAPHS_BY_ID)?.expect("paragraphs map");

    let mut cursors: Vec<(ID, Cursor)> = Vec::new();
    for key in map_keys(&paragraphs) {
      let Some(record) = child_map(&paragraphs, &key)? else {
        continue;
      };
      for field in ["boundary_cursor", "start_cursor"] {
        let Some(bytes) = map_binary_opt(&record, field)? else {
          continue;
        };
        let Ok(cursor) = Cursor::decode(&bytes) else {
          continue;
        };
        if cursor.container == body.id()
          && let Some(id) = cursor.id
        {
          cursors.push((id, cursor));
        }
      }
    }
    let ids: Vec<ID> = cursors.iter().map(|(id, _)| *id).collect();
    let batch = doc.inner().query_text_id_positions(&body.id(), &ids);
    assert_eq!(batch.len(), ids.len(), "one result per queried id");

    let mut checked = 0usize;
    for ((_, cursor), batch_pos) in cursors.iter().zip(&batch) {
      let per_cursor = doc.get_cursor_pos(cursor).ok().map(|result| result.current.pos);
      if let Some(pos) = batch_pos {
        assert_eq!(Some(*pos), per_cursor, "batch resolver disagreed with get_cursor_pos");
        checked += 1;
      }
    }
    assert!(checked >= 40, "expected >=40 live boundary cursors resolved by the batch path, got {checked}");
    Ok(())
  }

  /// Stronger equivalence: the batch resolver must still equal `get_cursor_pos`
  /// after fragmenting edits (insert/delete/re-insert) AND a concurrent multi-peer
  /// merge — the live collab state the fresh-doc test above doesn't reach. If the
  /// vendored resolver were wrong here, the full projection rebuild would assign
  /// different ids than the per-cursor path and show up as projection divergence.
  #[test]
  fn batch_resolver_matches_per_cursor_after_edits_and_merge() {
    use loro::{ExportMode, cursor::Side};
    let a = LoroDoc::new();
    a.set_peer_id(1).unwrap();
    let ta = a.get_text("t");
    ta.insert(0, "the quick brown fox jumps over").unwrap();
    a.commit();
    ta.delete(4, 6).unwrap(); // remove "quick "
    ta.insert(4, "SLOW ").unwrap();
    a.commit();
    ta.insert(0, "well, ").unwrap();
    a.commit();
    // Peer 2: snapshot from A, edit concurrently, merge back so `a` holds chunks
    // authored by two different peers.
    let b = LoroDoc::new();
    b.set_peer_id(2).unwrap();
    b.import(&a.export(ExportMode::Snapshot).unwrap()).unwrap();
    let tb = b.get_text("t");
    tb.insert(0, "PREFIX ").unwrap();
    let tb_end = tb.len_unicode();
    tb.insert(tb_end, " suffix").unwrap();
    b.commit();
    a.import(&b.export(ExportMode::updates(&a.oplog_vv())).unwrap()).unwrap();
    a.commit();

    let text = a.get_text("t");
    let container = text.id();
    let len = text.len_unicode();
    let mut ids: Vec<ID> = Vec::new();
    let mut cursors = Vec::new();
    for pos in 0..=len {
      for side in [Side::Left, Side::Right] {
        if let Some(cursor) = text.get_cursor(pos, side)
          && let Some(id) = cursor.id
        {
          ids.push(id);
          cursors.push(cursor);
        }
      }
    }
    assert!(!ids.is_empty(), "should have collected some cursors");

    let batch = a.inner().query_text_id_positions(&container, &ids);
    assert_eq!(batch.len(), ids.len());
    let mut compared = 0usize;
    for (cursor, batch_pos) in cursors.iter().zip(&batch) {
      let per_cursor = a.get_cursor_pos(cursor).ok().map(|result| result.current.pos);
      if let Some(pos) = batch_pos {
        assert_eq!(Some(*pos), per_cursor, "batch resolver diverged from get_cursor_pos on multi-peer/edited text");
        compared += 1;
      }
    }
    assert!(compared > 0, "should have compared at least some live cursors");
  }

  /// The fabricated/repair id derivation must be POSITION-INDEPENDENT: the same
  /// boundary newline keeps the same key after text is inserted ahead of it (it
  /// shifts position but its `OpID` is unchanged). This is what lets a fabricated id
  /// and a later-repaired record's id converge instead of diverging as the old
  /// `block_ix` / unicode-offset keys did.
  #[test]
  fn stable_boundary_keys_are_position_independent() -> io::Result<()> {
    let source = document_from_input_blocks(
      DocumentTheme::clone(&flowstate_document_theme()),
      vec![
        InputBlock::Paragraph(InputParagraph { style: gpui_flowtext::ParagraphStyle::Normal, runs: vec![InputRun { text: "alpha".into(), styles: RunStyles::default() }] }),
        InputBlock::Paragraph(InputParagraph { style: gpui_flowtext::ParagraphStyle::Normal, runs: vec![InputRun { text: "bravo".into(), styles: RunStyles::default() }] }),
      ],
    );
    let doc = document_to_loro(&source, "Stable boundary keys")?;
    let body = body_text(&doc);

    // A non-zero boundary (so it exercises the OpID path, not the boundary-0 seed).
    let boundary = body.to_string().chars().enumerate().filter_map(|(i, c)| (c == '\n').then_some(i)).find(|&i| i > 0).expect("a non-zero boundary");
    let before = stable_boundary_metadata_keys(&body, boundary).expect("keys for a live boundary");
    assert!(before.0.contains("op-") && before.1.contains("op-"), "non-zero boundary keys derive from the OpID: {before:?}");
    assert_ne!(before.0, before.1, "paragraph and block keys must be distinct");

    // Insert ahead of the boundary so it shifts by 4 unicode positions.
    body.insert(1, "XXXX").expect("insert");
    doc.commit();
    let after = stable_boundary_metadata_keys(&body, boundary + 4).expect("keys after shift");
    assert_eq!(before, after, "the same newline keeps the same key after shifting");
    Ok(())
  }

  #[test]
  fn projection_preserves_loro_paragraph_and_block_ids() -> io::Result<()> {
    let source = document_from_input_blocks(
      DocumentTheme::clone(&flowstate_document_theme()),
      vec![
        InputBlock::Paragraph(InputParagraph {
          style: gpui_flowtext::ParagraphStyle::Normal,
          runs: vec![InputRun {
            text: "before".to_string(),
            styles: RunStyles::default(),
          }],
        }),
        InputBlock::Image(InputImageBlock {
          asset_id: AssetId(42),
          alt_text: "alt".into(),
          caption: None,
          sizing: InputImageSizing::FitWidth,
          alignment: InputBlockAlignment::Left,
        }),
      ],
    );
    let doc = document_to_loro(&source, "Projection ids")?;
    let body = body_text(&doc);
    let root = doc.get_map(ROOT);
    let blocks = child_map(&root, BLOCKS_BY_ID)?.expect("blocks map");
    let first_paragraph_id = paragraph_ids_by_boundary(&doc, &body).get(&0).cloned().expect("first paragraph id");
    let first_block_id = paragraph_block_ids_by_boundary(&doc, &body).get(&0).cloned().expect("first paragraph block id");
    let image_id = map_keys(&blocks)
      .into_iter()
      .find(|key| {
        child_map(&blocks, key)
          .ok()
          .flatten()
          .and_then(|block| map_string_opt(&block, "kind").ok().flatten())
          .as_deref()
          == Some("image")
      })
      .expect("image block id");

    let projected = document_from_loro(&doc)?;

    assert_eq!(projected.ids.paragraph_ids[0], ParagraphId(loro_id_u128(&first_paragraph_id)));
    assert_eq!(projected.ids.block_ids[0], BlockId(loro_id_u128(&first_block_id)));
    assert_eq!(projected.ids.block_ids[1], BlockId(loro_id_u128(&image_id)));
    Ok(())
  }

  #[test]
  fn object_boundary_does_not_create_a_phantom_paragraph() -> io::Result<()> {
    let paragraph = |text: &str| {
      InputBlock::Paragraph(InputParagraph {
        style: gpui_flowtext::ParagraphStyle::Normal,
        runs: vec![InputRun {
          text: text.to_string(),
          styles: RunStyles::default(),
        }],
      })
    };
    let source = document_from_input_blocks(
      DocumentTheme::clone(&flowstate_document_theme()),
      vec![
        paragraph("before"),
        InputBlock::Image(InputImageBlock {
          asset_id: AssetId(7),
          alt_text: "figure".into(),
          caption: None,
          sizing: InputImageSizing::Intrinsic,
          alignment: InputBlockAlignment::Center,
        }),
        paragraph("after"),
      ],
    );

    let projected = document_from_loro(&document_to_loro(&source, "Object boundary")?)?;

    assert_eq!(projected.paragraphs.len(), 2);
    assert_eq!(gpui_flowtext::paragraph_text(&projected, 0), "before");
    assert_eq!(gpui_flowtext::paragraph_text(&projected, 1), "after");
    assert!(matches!(
      projected.blocks.to_vec().as_slice(),
      [
        gpui_flowtext::Block::Paragraph(_),
        gpui_flowtext::Block::Image(_),
        gpui_flowtext::Block::Paragraph(_)
      ]
    ));
    Ok(())
  }

  #[test]
  fn section_page_attrs_read_back_from_loro() {
    let doc = crate::loro_schema::new_loro_document("Sections").expect("new Loro document");
    let expected = crate::loro_schema::SectionPageAttrs {
      columns: 3,
      orientation: crate::loro_schema::SectionOrientation::Landscape,
      ..crate::loro_schema::SectionPageAttrs::default()
    };
    crate::loro_schema::set_section_page_attrs(&doc, "section.alpha", &expected).expect("set section page attrs");

    assert_eq!(section_page_attrs(&doc, "section.alpha"), Some(expected));
    assert_eq!(section_page_attrs(&doc, "section.missing"), None);
  }

  #[test]
  fn missing_paragraph_metadata_is_deterministic_and_quarantines_content() -> io::Result<()> {
    let source = document_from_input_blocks(
      DocumentTheme::clone(&flowstate_document_theme()),
      vec![InputBlock::Paragraph(InputParagraph {
        style: gpui_flowtext::ParagraphStyle::Normal,
        runs: vec![InputRun {
          text: "before".to_string(),
          styles: RunStyles::default(),
        }],
      })],
    );
    let doc = document_to_loro(&source, "Missing metadata")?;
    let body = body_text(&doc);
    // Append a second paragraph boundary that carries a style mark but has no
    // durable paragraph metadata record.
    let end = body.len_unicode();
    body.insert(end, "\nextra").unwrap();
    body.mark(end..end + 1, MARK_PARAGRAPH_STYLE, 0_i64).unwrap();
    doc.commit();

    let (projection, defects) = document_from_loro_with_defects(&doc)?;
    let (_, defects_again) = document_from_loro_with_defects(&doc)?;
    assert_eq!(defects, defects_again, "projection defects must be deterministic across passes");
    assert!(defects.iter().any(|defect| matches!(
      defect,
      ProjectionDefect::MissingParagraphMetadata {
        boundary_unicode: Some(_),
        ..
      }
    )));
    // The paragraph is quarantined with a fabricated id — its content survives.
    assert_eq!(projection.paragraphs.len(), 2);
    assert_eq!(gpui_flowtext::paragraph_text(&projection, 1), "extra");
    Ok(())
  }

  #[test]
  fn unresolved_object_anchor_is_quarantined_not_dropped() -> io::Result<()> {
    let source = document_from_input_blocks(
      DocumentTheme::clone(&flowstate_document_theme()),
      vec![InputBlock::Image(InputImageBlock {
        asset_id: AssetId(7),
        alt_text: "alt".into(),
        caption: None,
        sizing: InputImageSizing::FitWidth,
        alignment: InputBlockAlignment::Left,
      })],
    );
    let doc = document_to_loro(&source, "Unresolved anchor")?;
    let body = body_text(&doc);
    // Delete the object placeholder, leaving the block's anchor dangling.
    let placeholder = body
      .to_string()
      .chars()
      .position(|ch| ch == OBJECT_REPLACEMENT)
      .expect("object placeholder");
    body.delete(placeholder, 1).unwrap();
    doc.commit();

    let (projection, defects) = document_from_loro_with_defects(&doc)?;
    let (_, defects_again) = document_from_loro_with_defects(&doc)?;
    assert_eq!(defects, defects_again, "quarantine reporting must be deterministic");
    assert!(defects.iter().any(|defect| matches!(defect, ProjectionDefect::UnresolvedObjectAnchor { .. })));
    // The block is quarantined (appended), not silently dropped.
    assert!(projection.blocks.iter().any(|block| matches!(block, gpui_flowtext::Block::Image(_))));
    Ok(())
  }

  #[test]
  fn invalid_asset_id_is_reported_and_placeholdered() -> io::Result<()> {
    let source = document_from_input_blocks(
      DocumentTheme::clone(&flowstate_document_theme()),
      vec![InputBlock::Image(InputImageBlock {
        asset_id: AssetId(42),
        alt_text: "alt".into(),
        caption: None,
        sizing: InputImageSizing::FitWidth,
        alignment: InputBlockAlignment::Left,
      })],
    );
    let doc = document_to_loro(&source, "Invalid asset")?;
    let root = doc.get_map(ROOT);
    let blocks = child_map(&root, BLOCKS_BY_ID)?.expect("blocks map");
    let image_key = map_keys(&blocks)
      .into_iter()
      .find(|key| {
        child_map(&blocks, key)
          .ok()
          .flatten()
          .and_then(|block| map_string_opt(&block, "kind").ok().flatten())
          .as_deref()
          == Some("image")
      })
      .expect("image block");
    let image = child_map(&blocks, &image_key)?.expect("image block map");
    image.insert("asset_id", "not-a-number").unwrap();
    doc.commit();

    let (projection, defects) = document_from_loro_with_defects(&doc)?;
    assert!(defects.iter().any(|defect| matches!(defect, ProjectionDefect::InvalidAssetId { .. })));
    // Never silently coerced away: projected as the deterministic AssetId(0) placeholder.
    assert!(
      projection
        .blocks
        .iter()
        .any(|block| matches!(block, gpui_flowtext::Block::Image(image) if image.asset_id == AssetId(0)))
    );
    Ok(())
  }
}
