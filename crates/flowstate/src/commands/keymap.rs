use std::rc::Rc;

use gpui::{Action, App, DummyKeyboardMapper, KeyBinding, KeyBindingContextPredicate};
use serde::{Deserialize, Serialize};

use super::{COMMAND_SPECS, CommandId, FindInDocumentAction};
use crate::rich_text_element::{
  ApplyHighlightToSelection, Backspace, ClearFormatting, ClearHighlight, Copy, Cut, Delete, DeleteWordBackward, DeleteWordForward,
  InsertEquation, InsertImage, InsertNewline, InsertSoftLineBreak, InsertTable, MoveDocumentEnd, MoveDocumentStart, MoveDown, MoveLeft,
  MoveLineEnd, MoveLineStart, MoveRight, MoveUp, MoveWordLeft, MoveWordRight, PageDown, PageUp, Paste, Redo, Save, SelectAll, SelectDocumentEnd,
  SelectDocumentStart, SelectDown, SelectLeft, SelectLineEnd, SelectLineStart, SelectPageDown, SelectPageUp, SelectRight, SelectUp,
  SelectWordLeft, SelectWordRight, SetHighlightStyle1, SetParagraphStyle0, SetParagraphStyle1, SetParagraphStyle2, SetParagraphStyle3,
  SetParagraphStyle4, SetParagraphStyle6, ToggleSemanticStyle1, ToggleSemanticStyle2, ToggleStrikethrough, ToggleUnderline, Undo, ZoomIn,
  ZoomOut,
};

/// A complete keymap that can later be loaded from a structured user file.
///
/// For now this is built from `COMMAND_SPECS`. A future persisted keymap can
/// deserialize into this same shape, validate command IDs, then call
/// `register_keymap` without changing GPUI registration code.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Keymap {
  pub entries: Vec<KeymapEntry>,
}

#[hotpath::measure_all]
impl Keymap {
  pub fn defaults() -> Self {
    Self {
      entries: COMMAND_SPECS
        .iter()
        .flat_map(|spec| {
          spec.default_keys.iter().map(move |key| KeymapEntry {
            command: spec.id,
            key: (*key).to_string(),
            context: spec.context.map(str::to_string),
          })
        })
        .collect(),
    }
  }
}

/// One command binding in a keymap.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct KeymapEntry {
  pub command: CommandId,
  pub key: String,
  pub context: Option<String>,
}

/// Register the built-in default keymap.
#[hotpath::measure]
pub fn register_default_keybindings(cx: &mut App) {
  register_keymap(cx, &Keymap::defaults());
}

/// Register key bindings from a structured keymap object.
///
/// Unknown-to-GPUI command IDs are ignored here. That allows the command
/// catalog to contain app-level commands before those commands have GPUI action
/// types, while still letting editor commands be registered from the same data.
#[hotpath::measure]
pub fn register_keymap(cx: &mut App, keymap: &Keymap) {
  let bindings = keymap.entries.iter().filter_map(keybinding_for_entry);
  cx.bind_keys(bindings);
}

#[hotpath::measure]
fn keybinding_for_entry(entry: &KeymapEntry) -> Option<KeyBinding> {
  let context = entry
    .context
    .as_deref()
    .map(KeyBindingContextPredicate::parse)
    .transpose()
    .ok()?
    .map(Rc::new);
  KeyBinding::load(
    entry.key.as_str(),
    action_for_command(entry.command)?,
    context,
    false,
    None,
    &DummyKeyboardMapper,
  )
  .ok()
}

#[hotpath::measure]
pub(crate) fn action_for_command(command: CommandId) -> Option<Box<dyn Action>> {
  let action: Box<dyn Action> = match command {
    CommandId::MoveLeft => Box::new(MoveLeft),
    CommandId::MoveRight => Box::new(MoveRight),
    CommandId::MoveUp => Box::new(MoveUp),
    CommandId::MoveDown => Box::new(MoveDown),
    CommandId::MoveLineStart => Box::new(MoveLineStart),
    CommandId::MoveLineEnd => Box::new(MoveLineEnd),
    CommandId::SelectLeft => Box::new(SelectLeft),
    CommandId::SelectRight => Box::new(SelectRight),
    CommandId::SelectUp => Box::new(SelectUp),
    CommandId::SelectDown => Box::new(SelectDown),
    CommandId::SelectLineStart => Box::new(SelectLineStart),
    CommandId::SelectLineEnd => Box::new(SelectLineEnd),
    CommandId::SelectAll => Box::new(SelectAll),
    CommandId::MoveWordLeft => Box::new(MoveWordLeft),
    CommandId::MoveWordRight => Box::new(MoveWordRight),
    CommandId::SelectWordLeft => Box::new(SelectWordLeft),
    CommandId::SelectWordRight => Box::new(SelectWordRight),
    CommandId::DeleteWordBackward => Box::new(DeleteWordBackward),
    CommandId::DeleteWordForward => Box::new(DeleteWordForward),
    CommandId::PageUp => Box::new(PageUp),
    CommandId::PageDown => Box::new(PageDown),
    CommandId::SelectPageUp => Box::new(SelectPageUp),
    CommandId::SelectPageDown => Box::new(SelectPageDown),
    CommandId::MoveDocumentStart => Box::new(MoveDocumentStart),
    CommandId::MoveDocumentEnd => Box::new(MoveDocumentEnd),
    CommandId::SelectDocumentStart => Box::new(SelectDocumentStart),
    CommandId::SelectDocumentEnd => Box::new(SelectDocumentEnd),
    CommandId::Copy => Box::new(Copy),
    CommandId::Cut => Box::new(Cut),
    CommandId::Paste => Box::new(Paste),
    CommandId::Save => Box::new(Save),
    CommandId::Undo => Box::new(Undo),
    CommandId::Redo => Box::new(Redo),
    CommandId::SetParagraphPocket => Box::new(SetParagraphStyle0),
    CommandId::SetParagraphHat => Box::new(SetParagraphStyle1),
    CommandId::SetParagraphBlock => Box::new(SetParagraphStyle2),
    CommandId::SetParagraphTag => Box::new(SetParagraphStyle3),
    CommandId::SetParagraphAnalytic => Box::new(SetParagraphStyle4),
    CommandId::SetParagraphUndertag => Box::new(SetParagraphStyle6),
    CommandId::ToggleCite => Box::new(ToggleSemanticStyle1),
    CommandId::ToggleUnderline => Box::new(ToggleUnderline),
    CommandId::ToggleStrikethrough => Box::new(ToggleStrikethrough),
    CommandId::ToggleEmphasis => Box::new(ToggleSemanticStyle2),
    CommandId::SetHighlightSpoken => Box::new(SetHighlightStyle1),
    CommandId::ApplyHighlightToSelection => Box::new(ApplyHighlightToSelection),
    CommandId::ClearFormatting => Box::new(ClearFormatting),
    CommandId::ClearHighlight => Box::new(ClearHighlight),
    CommandId::InsertImage => Box::new(InsertImage),
    CommandId::InsertTable => Box::new(InsertTable),
    CommandId::InsertEquation => Box::new(InsertEquation),
    CommandId::ZoomIn => Box::new(ZoomIn),
    CommandId::ZoomOut => Box::new(ZoomOut),
    CommandId::Backspace => Box::new(Backspace),
    CommandId::Delete => Box::new(Delete),
    CommandId::InsertNewline => Box::new(InsertNewline),
    CommandId::InsertSoftLineBreak => Box::new(InsertSoftLineBreak),
    CommandId::FindInDocument => Box::new(FindInDocumentAction),
    CommandId::NewDocument
    | CommandId::OpenDocument
    | CommandId::OpenDemoDocument
    | CommandId::CloseDocument
    | CommandId::ShareDocument
    | CommandId::JoinSession
    | CommandId::StartCollaboration
    | CommandId::CopyCollaborationTicket
    | CommandId::JoinCollaborationFromClipboard
    | CommandId::LeaveCollaboration
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
    CommandId::FlowAddSiblingAbove | CommandId::FlowDeleteSelected | CommandId::FlowStrike => return None,
    CommandId::FlowNewFamily
    | CommandId::FlowNavigateUp
    | CommandId::FlowNavigateDown
    | CommandId::FlowNavigateLeft
    | CommandId::FlowNavigateRight
    | CommandId::FlowMoveUp
    | CommandId::FlowMoveDown
    | CommandId::FlowMoveLeft
    | CommandId::FlowMoveRight => return None,
  };
  Some(action)
}
