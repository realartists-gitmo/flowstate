pub mod instrument;
pub mod loro_import;
pub mod loro_projection;
pub mod loro_schema;
pub mod package;
mod package_search;
pub mod projection_defects;
pub mod table_topology;

pub use gpui_flowtext::*;
pub use loro_import::{
  ImportedLoroDocument, document_to_loro, import_document_projection, import_paragraphs_as_loro, write_imported_document_as_loro_db8,
};
pub use loro_projection::{document_from_loro, document_from_loro_with_defects, object_input_blocks_from_loro, section_page_attrs};
pub use projection_defects::{ProjectionDefect, TableTopologyKind};
pub use loro_schema::{
  BLOCKS_BY_ID, BODY_FLOW_ID, FLOW_ATTRS_KEY, FLOW_ID_KEY, FLOW_KIND_KEY, FLOW_TEXT_KEY, FLOWS_BY_ID, MAIN_BODY_BLOCK_ID, MARK_DIRECT_UNDERLINE,
  MARK_HIGHLIGHT_STYLE, MARK_PARAGRAPH_STYLE, MARK_RUN_SEMANTIC_STYLE, MARK_STRIKETHROUGH, META, OBJECT_REPLACEMENT, PARAGRAPHS_BY_ID,
  PageNumberFormat, REPLICAS_BY_ID, ROOT, ROOT_BODY_FLOW_ID, ROOT_FIRST_PARAGRAPH_ID, SECTION_ATTR_COLUMNS, SECTION_ATTR_FOOTER_FLOW_ID,
  SECTION_ATTR_HEADER_FLOW_ID, SECTION_ATTR_MARGIN_BOTTOM, SECTION_ATTR_MARGIN_LEFT, SECTION_ATTR_MARGIN_RIGHT, SECTION_ATTR_MARGIN_TOP,
  SECTION_ATTR_ORIENTATION, SECTION_ATTR_PAGE_HEIGHT, SECTION_ATTR_PAGE_NUMBERING_FORMAT, SECTION_ATTR_PAGE_NUMBERING_START,
  SECTION_ATTR_PAGE_WIDTH, SECTIONS_BY_ID, SENTINEL_NEWLINE, SectionMargins, SectionOrientation, SectionPageAttrs, SectionPageNumbering,
  SectionPageSize, TABLE_CELLS_BY_ID, TABLE_COLUMN_ORDER, TABLE_COLUMNS_BY_ID, TABLE_KEY, TABLE_ROW_ORDER, TABLE_ROWS_BY_ID, USERS_BY_ID,
  cell_flow_loro_id, cell_loro_id, cell_loro_id_for, column_loro_id, document_id, document_schema_version, ensure_section,
  fork_document_lineage, init_loro_document, new_loro_document, parse_cell_loro_id, parse_column_loro_id, parse_row_loro_id,
  read_section_page_attrs, record_revision, register_replica, register_user, row_loro_id, set_section_page_attrs, touch_document_metadata,
};
pub use package::{
  AssetChunk, ChunkRef, DEFAULT_UPDATE_SEGMENT_COMPACTION_THRESHOLD, DocumentPackage, DocumentPackageManifest, IntegrityIndexEntry,
  LORO_PACKAGE_FORMAT_VERSION, LORO_SCHEMA_VERSION, PackageRevision, ProjectionCacheChunk, SchemaMigrationRecord, SearchUnitChunk,
  ThumbnailChunk, loro_db8_bytes, read_loro_db8, write_loro_db8,
};

use std::{io, path::Path, sync::Arc};

use gpui::{Pixels, black, px, rgb};
use rustc_hash::{FxHashMap, FxHashSet};

pub const FLOWSTATE_EXTENSION: &str = "db8";

pub const PARAGRAPH_POCKET: ParagraphStyle = ParagraphStyle::Custom(0);
pub const PARAGRAPH_HAT: ParagraphStyle = ParagraphStyle::Custom(1);
pub const PARAGRAPH_BLOCK: ParagraphStyle = ParagraphStyle::Custom(2);
pub const PARAGRAPH_TAG: ParagraphStyle = ParagraphStyle::Custom(3);
pub const PARAGRAPH_ANALYTIC: ParagraphStyle = ParagraphStyle::Custom(4);
pub const PARAGRAPH_UNDERTAG: ParagraphStyle = ParagraphStyle::Custom(6);

pub const SEMANTIC_CITE: RunSemanticStyle = RunSemanticStyle::Custom(1);
pub const SEMANTIC_EMPHASIS: RunSemanticStyle = RunSemanticStyle::Custom(2);
pub const SEMANTIC_UNDERLINE: RunSemanticStyle = RunSemanticStyle::Custom(3);
pub const SEMANTIC_CONDENSED: RunSemanticStyle = RunSemanticStyle::Custom(4);
pub const SEMANTIC_ULTRACONDENSED: RunSemanticStyle = RunSemanticStyle::Custom(5);

pub const HIGHLIGHT_SPOKEN: HighlightStyle = HighlightStyle::Custom(1);
pub const HIGHLIGHT_INSERT: HighlightStyle = HighlightStyle::Custom(2);
pub const HIGHLIGHT_ALTERNATIVE: HighlightStyle = HighlightStyle::Custom(3);
pub const HIGHLIGHT_MARKED: HighlightStyle = HighlightStyle::Custom(4);

fn pt(value: f32) -> Pixels {
  px(value * 96.0 / 72.0)
}

fn border_eighth_points(value: f32) -> Pixels {
  pt(value / 8.0)
}

pub fn read_db8(path: impl AsRef<Path>) -> io::Result<DocumentProjection> {
  document_from_package(DocumentPackage::read(path)?)
}

pub fn read_db8_bytes(bytes: &[u8]) -> io::Result<DocumentProjection> {
  document_from_package(DocumentPackage::from_bytes(bytes)?)
}

fn document_from_package(package: DocumentPackage) -> io::Result<DocumentProjection> {
  let mut document = if let Some(document) = package.current_projection_document()? {
    document
  } else {
    document_from_loro(&package.load_loro_doc()?)?
  };
  attach_package_assets(&mut document, &package.assets);
  Ok(document)
}

pub fn attach_package_assets(document: &mut DocumentProjection, assets: &[AssetChunk]) {
  let referenced = referenced_asset_ids(document);
  for asset in assets
    .iter()
    .filter(|asset| referenced.contains(&AssetId(asset.asset_id)))
  {
    let bytes = asset.bytes.clone();
    document.assets.assets.insert(
      AssetId(asset.asset_id),
      AssetRecord {
        id: AssetId(asset.asset_id),
        mime_type: asset.mime_type.clone().into(),
        original_name: None,
        content_hash: AssetRecord::stable_content_hash(&bytes),
        bytes: Arc::new(bytes),
      },
    );
  }
  for id in referenced {
    document
      .assets
      .assets
      .entry(id)
      .or_insert_with(|| AssetRecord {
        id,
        mime_type: "application/octet-stream".into(),
        original_name: None,
        content_hash: AssetRecord::stable_content_hash(&[]),
        bytes: Arc::new(Vec::new()),
      });
  }
}

fn referenced_asset_ids(document: &DocumentProjection) -> FxHashSet<AssetId> {
  document
    .blocks
    .iter()
    .filter_map(|block| match block {
      Block::Image(image) => Some(image.asset_id),
      Block::Paragraph(_) | Block::Equation(_) | Block::Table(_) => None,
    })
    .collect()
}

pub fn flowstate_document_theme() -> DocumentTheme {
  let mut theme = DocumentTheme {
    zoom_factor: 1.0,
    default_font_family: "Carlito".into(),
    default_text_color: black(),
    document_background_color: rgb(0x00ff_ffff).into(),
    pageless_inset_x: px(24.0),
    pageless_inset_top: px(16.0),
    pageless_inset_bottom: px(24.0),
    body_font_size: pt(11.0),
    line_spacing: 259.0 / 240.0,
    line_gap_fraction: 0.18,
    paragraph_after: pt(8.0),
    inline_border_paint_width: px(0.5),
    box_padding_left: pt(0.96),
    box_padding_right: pt(1.01),
    box_padding_top: pt(1.47),
    box_padding_bottom: pt(1.09),
    highlight_pad_x: pt(0.0),
    highlight_top_extra_fraction: 0.22,
    highlight_bottom_extra_fraction: 0.092,
    underline_fallback_top_from_baseline: pt(1.246),
    underline_rule_thickness: px(1.0),
    snap_underline_rules_to_pixels: true,
    double_underline_top_from_baseline: pt(17.79 - 16.5),
    double_underline_gap: pt(1.20),
    default_highlight_color: rgb(0x00ff_f59d).into(),
    normal_bold: false,
    normal_italic: false,
    normal_underline: ThemeUnderline::None,
    custom_paragraph_styles: FxHashMap::default(),
    custom_semantic_styles: FxHashMap::default(),
    custom_highlight_styles: FxHashMap::default(),
    invisibility_visible_paragraph_styles: FxHashSet::default(),
    invisibility_visible_semantic_styles: FxHashSet::default(),
    invisibility_visible_highlight_styles: FxHashSet::default(),
  };

  theme.set_custom_paragraph_style(
    0,
    paragraph_style(
      pt(26.0),
      black(),
      true,
      false,
      ThemeUnderline::None,
      CustomParagraphAlign::Center,
      pt(12.0),
      px(0.0),
    )
    .with_border(border_eighth_points(24.0), pt(4.0), pt(1.0))
    .with_section(0, 0),
  );
  theme.set_custom_paragraph_style(
    1,
    paragraph_style(
      pt(22.0),
      black(),
      true,
      false,
      ThemeUnderline::Double,
      CustomParagraphAlign::Center,
      pt(2.0),
      px(0.0),
    )
    .with_section(1, 1),
  );
  theme.set_custom_paragraph_style(
    2,
    paragraph_style(
      pt(16.0),
      black(),
      true,
      false,
      ThemeUnderline::Single,
      CustomParagraphAlign::Center,
      pt(2.0),
      px(0.0),
    )
    .with_section(2, 2),
  );
  theme.set_custom_paragraph_style(
    3,
    paragraph_style(
      pt(13.0),
      black(),
      true,
      false,
      ThemeUnderline::None,
      CustomParagraphAlign::Left,
      pt(2.0),
      px(0.0),
    )
    .with_section(3, 3),
  );
  theme.set_custom_paragraph_style(
    4,
    paragraph_style(
      pt(13.0),
      rgb(0x001f_3864).into(),
      true,
      false,
      ThemeUnderline::None,
      CustomParagraphAlign::Left,
      pt(2.0),
      px(0.0),
    )
    .with_section(4, 3),
  );
  theme.set_custom_paragraph_style(
    6,
    paragraph_style(
      pt(12.0),
      rgb(0x0038_5623).into(),
      false,
      true,
      ThemeUnderline::None,
      CustomParagraphAlign::Left,
      px(0.0),
      px(0.0),
    )
    .into(),
  );

  theme.set_custom_semantic_style(
    1,
    CustomSemanticStyle {
      font_size: Some(pt(13.0)),
      color: Some(black()),
      bold: Some(true),
      italic: Some(false),
      underline: Some(ThemeUnderline::None),
      ..CustomSemanticStyle::default()
    },
  );
  theme.set_custom_semantic_style(
    2,
    CustomSemanticStyle {
      font_size: Some(pt(13.0)),
      color: Some(black()),
      bold: Some(true),
      italic: Some(false),
      underline: Some(ThemeUnderline::Single),
      border_width: Some(border_eighth_points(8.0)),
      ..CustomSemanticStyle::default()
    },
  );
  theme.set_custom_semantic_style(
    3,
    CustomSemanticStyle {
      font_size: Some(pt(11.0)),
      color: Some(black()),
      bold: Some(false),
      italic: Some(false),
      underline: Some(ThemeUnderline::Single),
      ..CustomSemanticStyle::default()
    },
  );
  theme.set_custom_semantic_style(
    4,
    CustomSemanticStyle {
      font_size: Some(pt(8.0)),
      color: Some(black()),
      bold: Some(false),
      italic: Some(false),
      underline: Some(ThemeUnderline::None),
      ..CustomSemanticStyle::default()
    },
  );
  theme.set_custom_semantic_style(
    5,
    CustomSemanticStyle {
      font_size: Some(pt(3.0)),
      color: Some(black()),
      bold: Some(false),
      italic: Some(false),
      underline: Some(ThemeUnderline::None),
      ..CustomSemanticStyle::default()
    },
  );

  theme.set_custom_highlight_style(
    1,
    CustomHighlightStyle {
      color: rgb(0x0000_ff00).into(),
    },
  );
  theme.set_custom_highlight_style(
    2,
    CustomHighlightStyle {
      color: rgb(0x00d9_d9d9).into(),
    },
  );
  theme.set_custom_highlight_style(
    3,
    CustomHighlightStyle {
      color: rgb(0x0000_ffff).into(),
    },
  );
  theme.set_custom_highlight_style(
    4,
    CustomHighlightStyle {
      color: rgb(0x00ff_0000).into(),
    },
  );
  for slot in [0, 1, 2, 3, 4, 6] {
    theme.set_invisibility_visible_paragraph_style(slot);
  }
  theme.set_invisibility_visible_semantic_style(1);
  theme.set_invisibility_visible_highlight_style(1);
  theme.set_invisibility_visible_highlight_style(3);
  theme
}

pub fn paragraph_slot(style: ParagraphStyle) -> Option<u8> {
  match style {
    ParagraphStyle::Normal => None,
    ParagraphStyle::Custom(slot) => Some(slot & 0x7f),
  }
}

pub fn semantic_slot(style: RunSemanticStyle) -> Option<u8> {
  match style {
    RunSemanticStyle::Plain => None,
    RunSemanticStyle::Custom(slot) => Some(slot & 0x7f),
  }
}

pub fn highlight_slot(style: HighlightStyle) -> u8 {
  match style {
    HighlightStyle::Custom(slot) => slot & 0x7f,
  }
}

pub fn custom_paragraph_style(theme: &DocumentTheme, slot: u8) -> CustomParagraphStyle {
  theme
    .custom_paragraph_styles
    .get(&(slot & 0x7f))
    .cloned()
    .unwrap_or_else(|| {
      let mut defaults = flowstate_document_theme();
      defaults
        .custom_paragraph_styles
        .remove(&(slot & 0x7f))
        .unwrap()
    })
}

pub fn set_custom_paragraph_style_value(theme: &mut DocumentTheme, slot: u8, style: CustomParagraphStyle) {
  let normalized = slot & 0x7f;
  theme.set_custom_paragraph_style(normalized, style);
}

pub fn custom_semantic_style(theme: &DocumentTheme, slot: u8) -> CustomSemanticStyle {
  theme
    .custom_semantic_styles
    .get(&(slot & 0x7f))
    .cloned()
    .unwrap_or_else(|| {
      let mut defaults = flowstate_document_theme();
      defaults
        .custom_semantic_styles
        .remove(&(slot & 0x7f))
        .unwrap_or_default()
    })
}

pub fn set_custom_semantic_style_value(theme: &mut DocumentTheme, slot: u8, style: CustomSemanticStyle) {
  let normalized = slot & 0x7f;
  theme.set_custom_semantic_style(normalized, style);
}

pub fn custom_highlight_color(theme: &DocumentTheme, slot: u8) -> gpui::Hsla {
  theme
    .custom_highlight_styles
    .get(&(slot & 0x7f))
    .map(|style| style.color)
    .unwrap_or_else(|| {
      let mut defaults = flowstate_document_theme();
      defaults
        .custom_highlight_styles
        .remove(&(slot & 0x7f))
        .map_or(theme.default_highlight_color, |style| style.color)
    })
}

pub fn set_custom_highlight_color(theme: &mut DocumentTheme, slot: u8, color: gpui::Hsla) {
  let normalized = slot & 0x7f;
  theme.set_custom_highlight_style(normalized, CustomHighlightStyle { color });
}

fn paragraph_style(
  font_size: Pixels,
  color: gpui::Hsla,
  bold: bool,
  italic: bool,
  underline: ThemeUnderline,
  align: CustomParagraphAlign,
  spacing_before: Pixels,
  spacing_after: Pixels,
) -> FlowstateParagraphStyleBuilder {
  FlowstateParagraphStyleBuilder(CustomParagraphStyle {
    font_size,
    font_family: None,
    color,
    bold,
    italic,
    underline,
    align,
    spacing_before,
    spacing_after,
    border: None,
    section_kind: None,
    section_level: None,
  })
}

struct FlowstateParagraphStyleBuilder(CustomParagraphStyle);

impl FlowstateParagraphStyleBuilder {
  fn with_border(mut self, width: Pixels, space_x: Pixels, space_y: Pixels) -> Self {
    self.0.border = Some(CustomParagraphBorder { width, space_x, space_y });
    self
  }

  fn with_section(mut self, kind: u8, level: u8) -> CustomParagraphStyle {
    self.0.section_kind = Some(kind);
    self.0.section_level = Some(level);
    self.0
  }
}

impl From<FlowstateParagraphStyleBuilder> for CustomParagraphStyle {
  fn from(builder: FlowstateParagraphStyleBuilder) -> Self {
    builder.0
  }
}
