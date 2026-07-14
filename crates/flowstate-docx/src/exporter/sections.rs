//! §11 page-structure export context (FS-126) and the shared unit converters
//! consumed by image fit-width sizing (FS-128) and table column widths (FS-124).
//!
//! A Flowstate [`DocumentProjection`] carries canonical page sections in
//! `document.sections`, each with an optional [`SectionPageAttrs`] payload
//! (`page`). docx-rs models the *final* section as document-level page
//! properties and every earlier section as a `w:sectPr` inside the last
//! paragraph of that section. This module resolves the effective attributes of
//! each section (its `page`, or the documented US-Letter defaults when absent)
//! and precomputes, per body block index, the section property that must be
//! attached at that boundary.

use docx_rs::{Docx, PageMargin, PageNumType, PageOrientationType, PageSize, SectionProperty};
use flowstate_document::{DocumentProjection, DocumentSection, block_ix_for_paragraph};
use flowstate_fidelity::FidelityClass;
use rustc_hash::FxHashMap;

/// EMUs per twip. 1 inch = 1440 twips = `914_400` EMU, so `914_400` / 1440 = 635.
const EMU_PER_TWIP: i64 = 635;
/// Twips per CSS pixel at 96 dpi. 1 inch = 1440 twips = 96 px, so 1440 / 96 = 15.
const TWIPS_PER_PX: i64 = 15;

/// US-Letter page width in twips (8.5in). Mirrors the canonical Loro default.
const DEFAULT_PAGE_WIDTH_TWIPS: i64 = 12_240;
/// US-Letter page height in twips (11in).
const DEFAULT_PAGE_HEIGHT_TWIPS: i64 = 15_840;
/// One-inch margin in twips.
const DEFAULT_MARGIN_TWIPS: i64 = 1_440;
/// Word's default header/footer distance from the page edge (0.5in).
const DEFAULT_HEADER_FOOTER_TWIPS: i32 = 720;

/// Convert twips to EMU, clamped to the `u32` domain docx-rs uses for extents.
pub(super) fn twips_to_emu(twips: i64) -> u32 {
  twips
    .max(0)
    .saturating_mul(EMU_PER_TWIP)
    .clamp(0, i64::from(u32::MAX)) as u32
}

/// Convert CSS pixels (96 dpi) to twips.
pub(super) fn px_to_twips(px: u32) -> i64 {
  i64::from(px) * TWIPS_PER_PX
}

/// Clamp a twip length into the `u32` docx-rs page-size domain.
fn twips_u32(twips: i64) -> u32 {
  twips.clamp(0, i64::from(u32::MAX)) as u32
}

/// Effective, projection-independent page attributes for one section. Reading
/// primitive fields here avoids naming `gpui_flowtext::SectionPageAttrs`, which
/// `flowstate-docx` cannot reference directly (the crate-root name resolves to
/// the distinct `loro_schema` mirror).
#[derive(Clone, Copy)]
pub(super) struct EffectiveSection {
  page_width_twips: i64,
  page_height_twips: i64,
  margin_top: i64,
  margin_right: i64,
  margin_bottom: i64,
  margin_left: i64,
  landscape: bool,
  page_num_enabled: bool,
  page_num_start: i64,
}

const DEFAULT_SECTION: EffectiveSection = EffectiveSection {
  page_width_twips: DEFAULT_PAGE_WIDTH_TWIPS,
  page_height_twips: DEFAULT_PAGE_HEIGHT_TWIPS,
  margin_top: DEFAULT_MARGIN_TWIPS,
  margin_right: DEFAULT_MARGIN_TWIPS,
  margin_bottom: DEFAULT_MARGIN_TWIPS,
  margin_left: DEFAULT_MARGIN_TWIPS,
  landscape: false,
  page_num_enabled: false,
  page_num_start: 1,
};

impl EffectiveSection {
  fn content_width_twips(&self) -> i64 {
    (self.page_width_twips - self.margin_left - self.margin_right).max(1)
  }

  fn page_size(&self) -> PageSize {
    let size = PageSize::new().size(twips_u32(self.page_width_twips), twips_u32(self.page_height_twips));
    if self.landscape {
      size.orient(PageOrientationType::Landscape)
    } else {
      size
    }
  }

  fn page_margin(&self) -> PageMargin {
    PageMargin::new()
      .top(self.margin_top as i32)
      .bottom(self.margin_bottom as i32)
      .left(self.margin_left as i32)
      .right(self.margin_right as i32)
      .header(DEFAULT_HEADER_FOOTER_TWIPS)
      .footer(DEFAULT_HEADER_FOOTER_TWIPS)
  }

  fn page_num_type(&self) -> Option<PageNumType> {
    (self.page_num_enabled || self.page_num_start != 1).then(|| PageNumType::new().start(self.page_num_start.max(0) as u32))
  }

  /// Build a paragraph-level `w:sectPr` for a non-final section boundary.
  fn section_property(&self) -> SectionProperty {
    let mut property = SectionProperty::new()
      .page_size(self.page_size())
      .page_margin(self.page_margin());
    if let Some(page_num_type) = self.page_num_type() {
      property = property.page_num_type(page_num_type);
    }
    property
  }
}

/// Read the effective attributes of a section: its canonical `page` payload, or
/// the documented defaults when the projection left it unset.
#[allow(
  clippy::default_trait_access,
  reason = "the gpui-flowtext section-attr enum types are not nameable from this crate; comparing against `Default::default()` classifies them without a path"
)]
fn effective_from(section: &DocumentSection) -> EffectiveSection {
  match &section.page {
    Some(page) => EffectiveSection {
      page_width_twips: page.page_size.width_twips,
      page_height_twips: page.page_size.height_twips,
      margin_top: page.margins.top_twips,
      margin_right: page.margins.right_twips,
      margin_bottom: page.margins.bottom_twips,
      margin_left: page.margins.left_twips,
      // Both enums derive `Default` (Portrait / None), so comparing against the
      // inferred default lets us classify them without naming the gpui-flowtext
      // types, which are not reachable from this crate.
      landscape: page.orientation != Default::default(),
      page_num_enabled: page.page_numbering.format != Default::default(),
      page_num_start: page.page_numbering.start,
    },
    None => DEFAULT_SECTION,
  }
}

/// Resolved page-structure context for one export.
pub(super) struct SectionContext {
  /// Attributes for the *final* section, applied at the document level.
  doc_level: EffectiveSection,
  /// Body block index -> section property to attach at that boundary.
  boundaries: FxHashMap<usize, EffectiveSection>,
  /// Document-wide content width (page width minus horizontal margins) used for
  /// image fit-width scaling and fractional table column widths. Taken from the
  /// first section for a stable single value.
  pub(super) content_width_twips: i64,
}

impl SectionContext {
  pub(super) fn resolve(document: &DocumentProjection) -> Self {
    if document.sections.is_empty() {
      return Self {
        doc_level: DEFAULT_SECTION,
        boundaries: FxHashMap::default(),
        content_width_twips: DEFAULT_SECTION.content_width_twips(),
      };
    }

    // Resolve each section to (start paragraph index, effective attrs). Sections
    // whose start paragraph no longer resolves are dropped from boundary
    // placement but still contribute their attrs to the ordered set.
    let mut resolved: Vec<(usize, EffectiveSection)> = Vec::with_capacity(document.sections.len());
    for section in document.sections.iter() {
      if let Some(start_ix) = document
        .ids
        .paragraph_ids
        .iter()
        .position(|id| *id == section.start_paragraph)
      {
        resolved.push((start_ix, effective_from(section)));
      }
    }

    if resolved.is_empty() {
      // FS-126 fidelity: no section start paragraph resolves, so only the first
      // section's geometry survives as the document-level sectPr; any further
      // sections drop their page geometry.
      flowstate_fidelity::check(
        document.sections.len() <= 1,
        FidelityClass::ImportExport,
        "export-dropped-section",
        || {
          format!(
            "{} canonical sections but none resolve a start paragraph; only one geometry written",
            document.sections.len()
          )
        },
      );
      let doc_level = effective_from(&document.sections[0]);
      return Self {
        content_width_twips: doc_level.content_width_twips(),
        doc_level,
        boundaries: FxHashMap::default(),
      };
    }

    resolved.sort_by_key(|(start_ix, _)| *start_ix);
    let content_width_twips = resolved[0].1.content_width_twips();
    // OOXML: the document-level `w:sectPr` describes the final section; every
    // earlier section terminates at the paragraph before the next section start.
    let doc_level = resolved[resolved.len() - 1].1;
    let mut boundaries = FxHashMap::default();
    for pair in resolved.windows(2) {
      let (_, this_section) = pair[0];
      let (next_start_ix, _) = pair[1];
      if let Some(last_paragraph_ix) = next_start_ix.checked_sub(1)
        && let Some(block_ix) = block_ix_for_paragraph(document, last_paragraph_ix)
      {
        boundaries.insert(block_ix, this_section);
      }
    }

    // FS-126 fidelity: every canonical section's page geometry must reach the
    // OOXML — the final section as the document-level sectPr and each earlier
    // section as a boundary sectPr. A section whose start paragraph or boundary
    // block no longer resolves (or whose boundary collides with another) drops
    // its geometry, so the written placement count falls below the section count.
    flowstate_fidelity::check(
      boundaries.len() + 1 >= document.sections.len(),
      FidelityClass::ImportExport,
      "export-dropped-section",
      || {
        format!(
          "{} canonical sections but only {} sectPr placements written (1 document-level + {} boundary)",
          document.sections.len(),
          boundaries.len() + 1,
          boundaries.len()
        )
      },
    );

    Self {
      doc_level,
      boundaries,
      content_width_twips,
    }
  }

  /// Apply the final section's page attributes at the document level.
  pub(super) fn apply_document_section(&self, mut docx: Docx) -> Docx {
    let section = self.doc_level;
    docx = docx.page_size(twips_u32(section.page_width_twips), twips_u32(section.page_height_twips));
    docx = docx.page_margin(section.page_margin());
    if section.landscape {
      docx = docx.page_orient(PageOrientationType::Landscape);
    }
    if let Some(page_num_type) = section.page_num_type() {
      docx = docx.page_num_type(page_num_type);
    }
    docx
  }

  /// The paragraph-level section property to attach when emitting the body block
  /// at `block_ix`, if it is a non-final section boundary.
  pub(super) fn boundary_section_property(&self, block_ix: usize) -> Option<SectionProperty> {
    self
      .boundaries
      .get(&block_ix)
      .map(EffectiveSection::section_property)
  }
}
