mod api;
mod collaboration;
mod demo;
mod document;
mod edit_ops;
mod persistence;
mod rich_text;

pub use api::*;
pub use collaboration::*;
pub use demo::*;
pub use document::*;
pub use edit_ops::*;
pub use persistence::*;
pub use rich_text::*;

pub mod prelude {
  pub use crate::{
    DocumentProjection, DocumentTheme, EditorSelection, HighlightStyle, Paragraph, ParagraphStyle, RichTextDocumentElement, RichTextEditor,
    RichTextEditorCommand, RunSemanticStyle, RunStyle, RunStyles, TextRun,
  };
}

pub mod style {
  pub use crate::{
    CustomHighlightStyle, CustomParagraphAlign, CustomParagraphBorder, CustomParagraphStyle, CustomSemanticStyle, HighlightStyle,
    HighlightStyleSpec, InlineStyleId, InlineStyleSpec, ParagraphStyle, RunSemanticStyle, StyleCatalog, StyleId, StyleSpec, ThemeUnderline,
  };
}

pub mod editor_api {
  pub use crate::{
    ArmedInlineTool, EditorEvent, EditorEventSink, EditorSelection, LayoutPolicy, RichTextDocumentElement, RichTextEditor,
    RichTextEditorCommand, RichTextEditorConfig, RichTextEditorStyleState, SaveStatus, SelectionState,
  };
}

pub mod host {
  pub use crate::{
    AssetResolver, BlockKindId, DocumentExportAdapter, DocumentExportFormat, DocumentRecoveryAdapter, DocumentSerializer,
    ExternalFormatExporter, set_document_export_adapter, set_document_recovery_adapter,
  };
}

pub mod advanced {
  pub use crate::collaboration::*;
  pub use crate::edit_ops::*;
}

