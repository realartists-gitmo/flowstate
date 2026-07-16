use std::{borrow::Cow, path::Path, sync::Arc};

use gpui::Pixels;

use crate::{
  DocumentExportFormat, DocumentProjection, EditorSelection, HighlightStyle, ParagraphStyle, RichTextEditorCommand, RunSemanticStyle,
};

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct StyleId(pub Cow<'static, str>);

impl StyleId {
  #[must_use]
  pub const fn borrowed(value: &'static str) -> Self {
    Self(Cow::Borrowed(value))
  }

  #[must_use]
  pub fn owned(value: impl Into<String>) -> Self {
    Self(Cow::Owned(value.into()))
  }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct InlineStyleId(pub Cow<'static, str>);

impl InlineStyleId {
  #[must_use]
  pub const fn borrowed(value: &'static str) -> Self {
    Self(Cow::Borrowed(value))
  }

  #[must_use]
  pub fn owned(value: impl Into<String>) -> Self {
    Self(Cow::Owned(value.into()))
  }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct BlockKindId(pub Cow<'static, str>);

impl BlockKindId {
  #[must_use]
  pub const fn borrowed(value: &'static str) -> Self {
    Self(Cow::Borrowed(value))
  }

  #[must_use]
  pub fn owned(value: impl Into<String>) -> Self {
    Self(Cow::Owned(value.into()))
  }
}

#[derive(Clone, Debug)]
pub struct StyleSpec {
  pub id: StyleId,
  pub label: Cow<'static, str>,
  pub shortcut_hint: Option<Cow<'static, str>>,
  pub style: ParagraphStyle,
}

#[derive(Clone, Debug)]
pub struct InlineStyleSpec {
  pub id: InlineStyleId,
  pub label: Cow<'static, str>,
  pub shortcut_hint: Option<Cow<'static, str>>,
  pub style: RunSemanticStyle,
}

#[derive(Clone, Debug)]
pub struct HighlightStyleSpec {
  pub id: InlineStyleId,
  pub label: Cow<'static, str>,
  pub shortcut_hint: Option<Cow<'static, str>>,
  pub style: HighlightStyle,
}

#[derive(Clone, Debug, Default)]
pub struct StyleCatalog {
  pub paragraph_styles: Vec<StyleSpec>,
  pub inline_styles: Vec<InlineStyleSpec>,
  pub highlight_styles: Vec<HighlightStyleSpec>,
}

#[derive(Clone, Debug)]
pub struct LayoutPolicy {
  pub foreground_overscan_px: Pixels,
  pub foreground_materialize_budget_ms: u64,
  pub max_chunk_lines: usize,
  pub scrollbar_drag_max_fps: usize,
  pub offscreen_layout_cache_overscan_paragraphs: usize,
  pub offscreen_prep_cache_overscan_paragraphs: usize,
}

impl Default for LayoutPolicy {
  fn default() -> Self {
    Self {
      foreground_overscan_px: gpui::px(384.0),
      foreground_materialize_budget_ms: 8,
      max_chunk_lines: 96,
      scrollbar_drag_max_fps: 60,
      offscreen_layout_cache_overscan_paragraphs: 24,
      offscreen_prep_cache_overscan_paragraphs: 160,
    }
  }
}

#[derive(Clone, Debug)]
pub enum EditorEvent {
  Changed {
    edit_generation: u64,
  },
  SelectionChanged {
    selection: EditorSelection,
  },
  CommandDispatched {
    command: RichTextEditorCommand,
  },
  Exported {
    format: DocumentExportFormat,
  },
  /// B-S1 (silent-refusal law): the editor declined a user action and the
  /// host must say so — e.g. an image that is too large or an unsupported
  /// format on insert. The message is user-facing.
  Refused {
    message: gpui::SharedString,
  },
  /// The editor could not reconcile optimistic local state with the canonical
  /// projection and recovered by discarding or rebuilding. Hosts must surface
  /// this (telemetry at minimum); it indicates replay/projection divergence.
  ReconciliationRecovery {
    dropped_batches: usize,
    reason: gpui::SharedString,
    total_recoveries: u64,
  },
}

pub trait EditorEventSink: Send + Sync + 'static {
  fn on_editor_event(&self, event: EditorEvent);
}

pub trait AssetResolver: Send + Sync + 'static {
  fn bytes_for_asset(&self, asset_id: crate::AssetId) -> Option<Arc<Vec<u8>>>;
}

pub trait DocumentSerializer: Send + Sync + 'static {
  fn read(&self, bytes: &[u8]) -> std::io::Result<DocumentProjection>;
  fn write(&self, document: &DocumentProjection) -> std::io::Result<Vec<u8>>;
}

pub trait ExternalFormatExporter: Send + Sync + 'static {
  fn write_external_format(&self, output_path: &Path, document: &DocumentProjection, format: DocumentExportFormat) -> std::io::Result<()>;
}
