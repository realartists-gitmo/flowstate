pub use flowstate_document::*;

use crate::commands::CommandId;

pub fn flowstate_command_to_rich_text(command: CommandId) -> Option<RichTextEditorCommand> {
  Some(match command {
    CommandId::MoveLeft => RichTextEditorCommand::MoveLeft,
    CommandId::MoveRight => RichTextEditorCommand::MoveRight,
    CommandId::MoveUp => RichTextEditorCommand::MoveUp,
    CommandId::MoveDown => RichTextEditorCommand::MoveDown,
    CommandId::MoveLineStart => RichTextEditorCommand::MoveLineStart,
    CommandId::MoveLineEnd => RichTextEditorCommand::MoveLineEnd,
    CommandId::SelectLeft => RichTextEditorCommand::SelectLeft,
    CommandId::SelectRight => RichTextEditorCommand::SelectRight,
    CommandId::SelectUp => RichTextEditorCommand::SelectUp,
    CommandId::SelectDown => RichTextEditorCommand::SelectDown,
    CommandId::SelectLineStart => RichTextEditorCommand::SelectLineStart,
    CommandId::SelectLineEnd => RichTextEditorCommand::SelectLineEnd,
    CommandId::SelectAll => RichTextEditorCommand::SelectAll,
    CommandId::MoveWordLeft => RichTextEditorCommand::MoveWordLeft,
    CommandId::MoveWordRight => RichTextEditorCommand::MoveWordRight,
    CommandId::SelectWordLeft => RichTextEditorCommand::SelectWordLeft,
    CommandId::SelectWordRight => RichTextEditorCommand::SelectWordRight,
    CommandId::DeleteWordBackward => RichTextEditorCommand::DeleteWordBackward,
    CommandId::DeleteWordForward => RichTextEditorCommand::DeleteWordForward,
    CommandId::PageUp => RichTextEditorCommand::PageUp,
    CommandId::PageDown => RichTextEditorCommand::PageDown,
    CommandId::SelectPageUp => RichTextEditorCommand::SelectPageUp,
    CommandId::SelectPageDown => RichTextEditorCommand::SelectPageDown,
    CommandId::MoveDocumentStart => RichTextEditorCommand::MoveDocumentStart,
    CommandId::MoveDocumentEnd => RichTextEditorCommand::MoveDocumentEnd,
    CommandId::SelectDocumentStart => RichTextEditorCommand::SelectDocumentStart,
    CommandId::SelectDocumentEnd => RichTextEditorCommand::SelectDocumentEnd,
    CommandId::Copy => RichTextEditorCommand::Copy,
    CommandId::Cut => RichTextEditorCommand::Cut,
    CommandId::Paste => RichTextEditorCommand::Paste,
    CommandId::Undo => RichTextEditorCommand::Undo,
    CommandId::Redo => RichTextEditorCommand::Redo,
    CommandId::SetParagraphPocket => RichTextEditorCommand::SetParagraphPocket,
    CommandId::SetParagraphHat => RichTextEditorCommand::SetParagraphHat,
    CommandId::SetParagraphBlock => RichTextEditorCommand::SetParagraphBlock,
    CommandId::SetParagraphTag => RichTextEditorCommand::SetParagraphTag,
    CommandId::SetParagraphAnalytic => RichTextEditorCommand::SetParagraphAnalytic,
    CommandId::SetParagraphUndertag => RichTextEditorCommand::SetParagraphUndertag,
    CommandId::ToggleCite => RichTextEditorCommand::ToggleCite,
    CommandId::ToggleUnderline => RichTextEditorCommand::ToggleUnderline,
    CommandId::ToggleStrikethrough => RichTextEditorCommand::ToggleStrikethrough,
    CommandId::ToggleEmphasis => RichTextEditorCommand::ToggleEmphasis,
    CommandId::SetHighlightSpoken => RichTextEditorCommand::SetHighlightSpoken,
    CommandId::ApplyHighlightToSelection => RichTextEditorCommand::ApplyHighlightToSelection,
    CommandId::ClearFormatting => RichTextEditorCommand::ClearFormatting,
    CommandId::ClearHighlight => RichTextEditorCommand::ClearHighlight,
    CommandId::InsertImage => RichTextEditorCommand::InsertImage,
    CommandId::InsertTable => RichTextEditorCommand::InsertTable,
    CommandId::InsertEquation => RichTextEditorCommand::InsertEquation,
    CommandId::ZoomIn => RichTextEditorCommand::ZoomIn,
    CommandId::ZoomOut => RichTextEditorCommand::ZoomOut,
    CommandId::Backspace => RichTextEditorCommand::Backspace,
    CommandId::Delete => RichTextEditorCommand::Delete,
    CommandId::InsertNewline => RichTextEditorCommand::InsertNewline,
    CommandId::InsertSoftLineBreak => RichTextEditorCommand::InsertSoftLineBreak,
    CommandId::Save
    | CommandId::NewDocument
    | CommandId::OpenDocument
    | CommandId::OpenDemoDocument
    | CommandId::CloseDocument
    | CommandId::ToggleRibbon
    | CommandId::ScrollToParagraph => return None,
  })
}
