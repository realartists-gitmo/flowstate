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
    CommandId::SetParagraphPocket => RichTextEditorCommand::SetParagraphStyle(0),
    CommandId::SetParagraphHat => RichTextEditorCommand::SetParagraphStyle(1),
    CommandId::SetParagraphBlock => RichTextEditorCommand::SetParagraphStyle(2),
    CommandId::SetParagraphTag => RichTextEditorCommand::SetParagraphStyle(3),
    CommandId::SetParagraphAnalytic => RichTextEditorCommand::SetParagraphStyle(4),
    CommandId::SetParagraphUndertag => RichTextEditorCommand::SetParagraphStyle(6),
    CommandId::ToggleCite => RichTextEditorCommand::ToggleSemanticStyle(1),
    CommandId::ToggleUnderline => RichTextEditorCommand::ToggleUnderline,
    CommandId::ToggleStrikethrough => RichTextEditorCommand::ToggleStrikethrough,
    CommandId::ToggleEmphasis => RichTextEditorCommand::ToggleSemanticStyle(2),
    CommandId::SetHighlightSpoken => RichTextEditorCommand::SetHighlightStyle(1),
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
    | CommandId::FindInDocument
    | CommandId::ToggleRibbon
    | CommandId::NextTab
    | CommandId::PreviousTab
    | CommandId::TogglePinTab
    | CommandId::SendToSpeechDocument
    | CommandId::SendToSpeechDocumentEnd
    | CommandId::CondenseSelection
    | CommandId::CondensedSelection
    | CommandId::ToggleSpeechDocument
    | CommandId::ExportFormat
    | CommandId::ExportSend
    | CommandId::ToggleInvisibility
    | CommandId::MarkCard
    | CommandId::SwitchToTab1
    | CommandId::SwitchToTab2
    | CommandId::SwitchToTab3
    | CommandId::SwitchToTab4
    | CommandId::SwitchToTab5
    | CommandId::SwitchToTab6
    | CommandId::SwitchToTab7
    | CommandId::SwitchToTab8
    | CommandId::SwitchToTab9
    | CommandId::SwitchToTab10
    | CommandId::ScrollToParagraph => return None,
  })
}
