pub use flowstate_document::*;

use crate::commands::CommandId;
use gpui::{Pixels, black, px, rgb};

fn pt(value: f32) -> Pixels {
  px(value * 96.0 / 72.0)
}

fn border_eighth_points(value: f32) -> Pixels {
  pt(value / 8.0)
}

pub fn flowstate_document_theme() -> DocumentTheme {
  let mut theme = DocumentTheme::default();
  theme.default_font_family = "Carlito".into();
  theme.body_font_size = pt(11.0);
  theme.cite_font_size = pt(13.0);
  theme.condensed_font_size = pt(8.0);
  theme.ultracondensed_font_size = pt(3.0);
  theme.pocket_font_size = pt(26.0);
  theme.hat_font_size = pt(22.0);
  theme.block_font_size = pt(16.0);
  theme.tag_font_size = pt(13.0);
  theme.undertag_font_size = pt(12.0);
  theme.line_spacing = 259.0 / 240.0;
  theme.line_gap_fraction = 0.18;
  theme.paragraph_after = pt(8.0);
  theme.pocket_before = pt(12.0);
  theme.hat_before = pt(2.0);
  theme.block_before = pt(2.0);
  theme.tag_before = pt(2.0);
  theme.pocket_border_width = border_eighth_points(24.0);
  theme.pocket_border_space_x = pt(4.0);
  theme.pocket_border_space_y = pt(1.0);
  theme.emphasis_border_width = border_eighth_points(8.0);
  theme.emphasis_border_paint_width = px(0.5);
  theme.box_padding_left = pt(0.96);
  theme.box_padding_right = pt(1.01);
  theme.box_padding_top = pt(1.47);
  theme.box_padding_bottom = pt(1.09);
  theme.highlight_pad_x = pt(0.0);
  theme.highlight_top_extra_fraction = 0.22;
  theme.highlight_bottom_extra_fraction = 0.092;
  theme.underline_fallback_top_from_baseline = pt(1.246);
  theme.underline_rule_thickness = px(1.0);
  theme.snap_underline_rules_to_pixels = true;
  theme.double_underline_top_from_baseline = pt(17.79 - 16.5);
  theme.double_underline_gap = pt(1.20);
  theme.highlight_spoken = rgb(0x0000_ff00).into();
  theme.highlight_insert = rgb(0x00d9_d9d9).into();
  theme.highlight_alternative = rgb(0x0000_ffff).into();
  theme.pocket_color = black();
  theme.hat_color = black();
  theme.block_color = black();
  theme.tag_color = black();
  theme.analytic_color = rgb(0x001f_3864).into();
  theme.undertag_color = rgb(0x0038_5623).into();
  theme.cite_color = black();
  theme.underline_color = black();
  theme.emphasis_color = black();
  theme.condensed_color = black();
  theme.ultracondensed_color = black();
  theme.normal_bold = false;
  theme.normal_italic = false;
  theme.normal_underline = ThemeUnderline::None;
  theme.pocket_bold = true;
  theme.pocket_italic = false;
  theme.pocket_underline = ThemeUnderline::None;
  theme.hat_bold = true;
  theme.hat_italic = false;
  theme.hat_underline = ThemeUnderline::Double;
  theme.block_bold = true;
  theme.block_italic = false;
  theme.block_underline = ThemeUnderline::Single;
  theme.tag_bold = true;
  theme.tag_italic = false;
  theme.tag_underline = ThemeUnderline::None;
  theme.analytic_bold = true;
  theme.analytic_italic = false;
  theme.analytic_underline = ThemeUnderline::None;
  theme.undertag_bold = false;
  theme.undertag_italic = true;
  theme.undertag_underline = ThemeUnderline::None;
  theme.cite_bold = true;
  theme.cite_italic = false;
  theme.cite_underline = ThemeUnderline::None;
  theme.underline_bold = false;
  theme.underline_italic = false;
  theme.underline_underline = ThemeUnderline::Single;
  theme.emphasis_bold = true;
  theme.emphasis_italic = false;
  theme.emphasis_underline = ThemeUnderline::Single;
  theme.condensed_bold = false;
  theme.condensed_italic = false;
  theme.condensed_underline = ThemeUnderline::None;
  theme.ultracondensed_bold = false;
  theme.ultracondensed_italic = false;
  theme.ultracondensed_underline = ThemeUnderline::None;
  theme
}

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
