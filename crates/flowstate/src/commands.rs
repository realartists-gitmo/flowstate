mod keymap;

use gpui::actions;

pub(crate) use keymap::action_for_command;
pub use keymap::{Keymap, KeymapEntry, register_default_keybindings, register_keymap};

actions!(flowstate_workspace, [FindInDocumentAction, FidelityMarkAction]);

pub const RICH_TEXT_CONTEXT: &str = "RichTextEditor";

/// Stable user-facing command identifier.
///
/// IMPORTANT: every function users can trigger from the keyboard, menus,
/// ribbon, toolbar, context menu, command palette, or future scripting API must
/// have an entry here. UI code should route through these IDs instead of
/// inventing one-off button handlers that cannot be rebound or displayed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, serde::Deserialize, serde::Serialize)]
pub enum CommandId {
  MoveLeft,
  MoveRight,
  MoveUp,
  MoveDown,
  MoveLineStart,
  MoveLineEnd,
  SelectLeft,
  SelectRight,
  SelectUp,
  SelectDown,
  SelectLineStart,
  SelectLineEnd,
  SelectAll,
  MoveWordLeft,
  MoveWordRight,
  SelectWordLeft,
  SelectWordRight,
  DeleteWordBackward,
  DeleteWordForward,
  PageUp,
  PageDown,
  SelectPageUp,
  SelectPageDown,
  MoveDocumentStart,
  MoveDocumentEnd,
  SelectDocumentStart,
  SelectDocumentEnd,
  Copy,
  Cut,
  Paste,
  Save,
  Undo,
  Redo,
  SetParagraphPocket,
  SetParagraphHat,
  SetParagraphBlock,
  SetParagraphTag,
  SetParagraphAnalytic,
  SetParagraphUndertag,
  ToggleCite,
  ToggleUnderline,
  ToggleStrikethrough,
  ToggleEmphasis,
  SetHighlightSpoken,
  ApplyHighlightToSelection,
  ClearFormatting,
  ClearHighlight,
  InsertImage,
  InsertTable,
  InsertEquation,
  ZoomIn,
  ZoomOut,
  Backspace,
  Delete,
  InsertNewline,
  InsertSoftLineBreak,
  NewDocument,
  OpenDocument,
  OpenDemoDocument,
  CloseDocument,
  ShareDocument,
  JoinSession,
  StartCollaboration,
  CopyCollaborationTicket,
  JoinCollaborationFromClipboard,
  LeaveCollaboration,
  FindInDocument,
  ToggleRibbon,
  NextTab,
  PreviousTab,
  TogglePinTab,
  SendToSpeechDocument,
  SendToSpeechDocumentEnd,
  CondenseSelection,
  MarkCard,
  CondensedSelection,
  ToggleSpeechDocument,
  ExportFormat,
  ExportSend,
  ToggleInvisibility,
  SwitchToTab1,
  SwitchToTab2,
  SwitchToTab3,
  SwitchToTab4,
  SwitchToTab5,
  SwitchToTab6,
  SwitchToTab7,
  SwitchToTab8,
  SwitchToTab9,
  SwitchToTab10,
  ScrollToParagraph,
  FlowAddSiblingAbove,
  FlowDeleteSelected,
  FlowStrike,
  FlowNewFamily,
  FlowNavigateUp,
  FlowNavigateDown,
  FlowNavigateLeft,
  FlowNavigateRight,
  FlowMoveUp,
  FlowMoveDown,
  FlowMoveLeft,
  FlowMoveRight,
}

#[derive(Clone, Copy, Debug)]
pub struct CommandSpec {
  pub id: CommandId,
  pub label: &'static str,
  pub context: Option<&'static str>,
  pub default_keys: &'static [&'static str],
}

impl CommandSpec {
  pub const fn new(id: CommandId, label: &'static str, context: Option<&'static str>, default_keys: &'static [&'static str]) -> Self {
    Self {
      id,
      label,
      context,
      default_keys,
    }
  }
}

const EDITOR: Option<&str> = Some(RICH_TEXT_CONTEXT);
const APP: Option<&str> = None;

/// Metadata source for command palette, menus, rebinding UI, toolbar labels,
/// and "show shortcut for command" UI.
///
/// This currently exposes default bindings. When custom keymaps are added,
/// keep this as the stable command catalog and layer user overrides on top.
pub const COMMAND_SPECS: &[CommandSpec] = &[
  CommandSpec::new(CommandId::MoveLeft, "Move Left", EDITOR, &["left"]),
  CommandSpec::new(CommandId::MoveRight, "Move Right", EDITOR, &["right"]),
  CommandSpec::new(CommandId::MoveUp, "Move Up", EDITOR, &["up"]),
  CommandSpec::new(CommandId::MoveDown, "Move Down", EDITOR, &["down"]),
  CommandSpec::new(CommandId::MoveLineStart, "Move to Line Start", EDITOR, &["home"]),
  CommandSpec::new(CommandId::MoveLineEnd, "Move to Line End", EDITOR, &["end"]),
  CommandSpec::new(CommandId::SelectLeft, "Select Left", EDITOR, &["shift-left"]),
  CommandSpec::new(CommandId::SelectRight, "Select Right", EDITOR, &["shift-right"]),
  CommandSpec::new(CommandId::SelectUp, "Select Up", EDITOR, &["shift-up"]),
  CommandSpec::new(CommandId::SelectDown, "Select Down", EDITOR, &["shift-down"]),
  CommandSpec::new(CommandId::SelectLineStart, "Select to Line Start", EDITOR, &["shift-home"]),
  CommandSpec::new(CommandId::SelectLineEnd, "Select to Line End", EDITOR, &["shift-end"]),
  CommandSpec::new(CommandId::SelectAll, "Select All", EDITOR, &["cmd-a", "ctrl-a"]),
  CommandSpec::new(CommandId::MoveWordLeft, "Move Word Left", EDITOR, &["ctrl-left", "alt-left"]),
  CommandSpec::new(CommandId::MoveWordRight, "Move Word Right", EDITOR, &["ctrl-right", "alt-right"]),
  CommandSpec::new(
    CommandId::SelectWordLeft,
    "Select Word Left",
    EDITOR,
    &["ctrl-shift-left", "alt-shift-left"],
  ),
  CommandSpec::new(
    CommandId::SelectWordRight,
    "Select Word Right",
    EDITOR,
    &["ctrl-shift-right", "alt-shift-right"],
  ),
  CommandSpec::new(CommandId::DeleteWordBackward, "Delete Word Backward", EDITOR, &["ctrl-backspace"]),
  CommandSpec::new(CommandId::DeleteWordForward, "Delete Word Forward", EDITOR, &["ctrl-delete"]),
  CommandSpec::new(CommandId::PageUp, "Page Up", EDITOR, &["pageup"]),
  CommandSpec::new(CommandId::PageDown, "Page Down", EDITOR, &["pagedown"]),
  CommandSpec::new(CommandId::SelectPageUp, "Select Page Up", EDITOR, &["shift-pageup"]),
  CommandSpec::new(CommandId::SelectPageDown, "Select Page Down", EDITOR, &["shift-pagedown"]),
  CommandSpec::new(CommandId::MoveDocumentStart, "Move to Document Start", EDITOR, &["ctrl-home"]),
  CommandSpec::new(CommandId::MoveDocumentEnd, "Move to Document End", EDITOR, &["ctrl-end"]),
  CommandSpec::new(CommandId::SelectDocumentStart, "Select to Document Start", EDITOR, &["ctrl-shift-home"]),
  CommandSpec::new(CommandId::SelectDocumentEnd, "Select to Document End", EDITOR, &["ctrl-shift-end"]),
  CommandSpec::new(CommandId::Copy, "Copy", EDITOR, &["cmd-c", "ctrl-c"]),
  CommandSpec::new(CommandId::Cut, "Cut", EDITOR, &["cmd-x", "ctrl-x"]),
  CommandSpec::new(CommandId::Paste, "Paste", EDITOR, &["cmd-v", "ctrl-v"]),
  CommandSpec::new(CommandId::Save, "Save", EDITOR, &["cmd-s", "ctrl-s"]),
  CommandSpec::new(CommandId::Undo, "Undo", EDITOR, &["cmd-z", "ctrl-z"]),
  CommandSpec::new(CommandId::Redo, "Redo", EDITOR, &["cmd-shift-z", "ctrl-shift-z", "ctrl-y"]),
  CommandSpec::new(CommandId::SetParagraphPocket, "Set Paragraph: Pocket", EDITOR, &["f4"]),
  CommandSpec::new(CommandId::SetParagraphHat, "Set Paragraph: Hat", EDITOR, &["f5"]),
  CommandSpec::new(CommandId::SetParagraphBlock, "Set Paragraph: Block", EDITOR, &["f6"]),
  CommandSpec::new(CommandId::SetParagraphTag, "Set Paragraph: Tag", EDITOR, &["f7"]),
  CommandSpec::new(CommandId::SetParagraphAnalytic, "Set Paragraph: Analytic", EDITOR, &["ctrl-f7"]),
  CommandSpec::new(CommandId::SetParagraphUndertag, "Set Paragraph: Undertag", EDITOR, &["ctrl-f8"]),
  CommandSpec::new(CommandId::ToggleCite, "Toggle Cite", EDITOR, &["f8"]),
  CommandSpec::new(CommandId::ToggleUnderline, "Toggle Underline", EDITOR, &["f9", "cmd-u", "ctrl-u"]),
  CommandSpec::new(CommandId::ToggleStrikethrough, "Toggle Strikethrough", EDITOR, &[]),
  CommandSpec::new(CommandId::ToggleEmphasis, "Toggle Emphasis", EDITOR, &["f10", "cmd-b", "ctrl-b"]),
  CommandSpec::new(CommandId::SetHighlightSpoken, "Set Highlight: Spoken", EDITOR, &[]),
  CommandSpec::new(CommandId::ApplyHighlightToSelection, "Apply Highlight to Selection", EDITOR, &["f11"]),
  CommandSpec::new(CommandId::ClearFormatting, "Clear Formatting", EDITOR, &["f12"]),
  CommandSpec::new(CommandId::ClearHighlight, "Clear Highlight", EDITOR, &["ctrl-shift-h"]),
  CommandSpec::new(CommandId::InsertImage, "Insert Image", EDITOR, &[]),
  CommandSpec::new(CommandId::InsertTable, "Insert Table", EDITOR, &[]),
  CommandSpec::new(CommandId::InsertEquation, "Insert Equation", EDITOR, &[]),
  CommandSpec::new(CommandId::ZoomIn, "Zoom In", APP, &["ctrl-+", "ctrl-="]),
  CommandSpec::new(CommandId::ZoomOut, "Zoom Out", APP, &["ctrl--"]),
  CommandSpec::new(CommandId::Backspace, "Backspace", EDITOR, &["backspace"]),
  CommandSpec::new(CommandId::Delete, "Delete", EDITOR, &["delete"]),
  CommandSpec::new(CommandId::InsertNewline, "Insert Paragraph Break", EDITOR, &["enter"]),
  CommandSpec::new(CommandId::InsertSoftLineBreak, "Insert Soft Line Break", EDITOR, &["shift-enter"]),
  CommandSpec::new(CommandId::NewDocument, "New Document", APP, &[]),
  CommandSpec::new(CommandId::OpenDocument, "Open Document", APP, &[]),
  CommandSpec::new(CommandId::OpenDemoDocument, "Open Demo Document", APP, &[]),
  CommandSpec::new(CommandId::CloseDocument, "Close Document", APP, &[]),
  CommandSpec::new(CommandId::ShareDocument, "Share...", APP, &[]),
  CommandSpec::new(CommandId::JoinSession, "Join Collaboration Session...", APP, &[]),
  CommandSpec::new(CommandId::StartCollaboration, "Start Collaboration", APP, &[]),
  CommandSpec::new(CommandId::CopyCollaborationTicket, "Copy Collaboration Invite", APP, &[]),
  CommandSpec::new(CommandId::JoinCollaborationFromClipboard, "Join Collaboration from Clipboard", APP, &[]),
  CommandSpec::new(CommandId::LeaveCollaboration, "Leave Collaboration", APP, &[]),
  CommandSpec::new(CommandId::FindInDocument, "Find in Document", APP, &["cmd-f", "ctrl-f"]),
  CommandSpec::new(CommandId::ToggleRibbon, "Toggle Ribbon", APP, &[]),
  CommandSpec::new(CommandId::NextTab, "Next Tab", APP, &["ctrl-tab"]),
  CommandSpec::new(CommandId::PreviousTab, "Previous Tab", APP, &["ctrl-shift-tab"]),
  CommandSpec::new(CommandId::TogglePinTab, "Toggle Pin Tab", APP, &["ctrl-shift-p"]),
  CommandSpec::new(CommandId::SendToSpeechDocument, "Send to Speech Document", APP, &["`"]),
  CommandSpec::new(CommandId::SendToSpeechDocumentEnd, "Send to Speech Document End", APP, &["alt-`"]),
  CommandSpec::new(CommandId::CondenseSelection, "Condense Selection", APP, &["f3"]),
  CommandSpec::new(CommandId::MarkCard, "Mark Card", APP, &["ctrl-m"]),
  CommandSpec::new(CommandId::CondensedSelection, "Shrink", APP, &["ctrl-8"]),
  CommandSpec::new(CommandId::ToggleSpeechDocument, "Toggle Speech Document", APP, &[]),
  CommandSpec::new(CommandId::ExportFormat, "Export Format", APP, &[]),
  CommandSpec::new(CommandId::ExportSend, "Export Send", APP, &[]),
  CommandSpec::new(CommandId::ToggleInvisibility, "Toggle Invisibility", APP, &[]),
  CommandSpec::new(CommandId::SwitchToTab1, "Switch to Tab 1", APP, &["alt-1"]),
  CommandSpec::new(CommandId::SwitchToTab2, "Switch to Tab 2", APP, &["alt-2"]),
  CommandSpec::new(CommandId::SwitchToTab3, "Switch to Tab 3", APP, &["alt-3"]),
  CommandSpec::new(CommandId::SwitchToTab4, "Switch to Tab 4", APP, &["alt-4"]),
  CommandSpec::new(CommandId::SwitchToTab5, "Switch to Tab 5", APP, &["alt-5"]),
  CommandSpec::new(CommandId::SwitchToTab6, "Switch to Tab 6", APP, &["alt-6"]),
  CommandSpec::new(CommandId::SwitchToTab7, "Switch to Tab 7", APP, &["alt-7"]),
  CommandSpec::new(CommandId::SwitchToTab8, "Switch to Tab 8", APP, &["alt-8"]),
  CommandSpec::new(CommandId::SwitchToTab9, "Switch to Tab 9", APP, &["alt-9"]),
  CommandSpec::new(CommandId::SwitchToTab10, "Switch to Tab 10", APP, &["alt-0"]),
  CommandSpec::new(CommandId::ScrollToParagraph, "Scroll to Paragraph", APP, &[]),
  CommandSpec::new(CommandId::FlowAddSiblingAbove, "Flow: Add Sibling Above", APP, &["alt-enter"]),
  CommandSpec::new(CommandId::FlowDeleteSelected, "Flow: Delete Selected", APP, &["cmd-delete"]),
  CommandSpec::new(CommandId::FlowStrike, "Flow: Strike", APP, &["cmd-shift-x", "ctrl-shift-x"]),
  CommandSpec::new(CommandId::FlowNewFamily, "Flow: New Family", APP, &["cmd-enter", "ctrl-enter"]),
  CommandSpec::new(CommandId::FlowNavigateUp, "Flow: Navigate Up", APP, &["ctrl-up"]),
  CommandSpec::new(CommandId::FlowNavigateDown, "Flow: Navigate Down", APP, &["ctrl-down"]),
  CommandSpec::new(CommandId::FlowNavigateLeft, "Flow: Navigate Left", APP, &["ctrl-left"]),
  CommandSpec::new(CommandId::FlowNavigateRight, "Flow: Navigate Right", APP, &["ctrl-right"]),
  CommandSpec::new(CommandId::FlowMoveUp, "Flow: Move Up", APP, &["shift-alt-up"]),
  CommandSpec::new(CommandId::FlowMoveDown, "Flow: Move Down", APP, &["shift-alt-down"]),
  CommandSpec::new(CommandId::FlowMoveLeft, "Flow: Move Left", APP, &["shift-alt-left"]),
  CommandSpec::new(CommandId::FlowMoveRight, "Flow: Move Right", APP, &["shift-alt-right"]),
];

#[hotpath::measure]
pub fn command_spec(id: CommandId) -> Option<&'static CommandSpec> {
  COMMAND_SPECS.iter().find(|spec| spec.id == id)
}

pub const RIBBON_KEYMAP_COMMANDS: &[CommandId] = &[
  CommandId::SetParagraphPocket,
  CommandId::SetParagraphHat,
  CommandId::SetParagraphBlock,
  CommandId::SetParagraphTag,
  CommandId::SetParagraphAnalytic,
  CommandId::SetParagraphUndertag,
  CommandId::ToggleCite,
  CommandId::ToggleEmphasis,
  CommandId::ToggleUnderline,
  CommandId::ToggleStrikethrough,
  CommandId::CondenseSelection,
  CommandId::CondensedSelection,
  CommandId::ApplyHighlightToSelection,
  CommandId::ClearHighlight,
  CommandId::ClearFormatting,
  CommandId::MarkCard,
  CommandId::ToggleSpeechDocument,
  CommandId::SendToSpeechDocument,
  CommandId::SendToSpeechDocumentEnd,
  CommandId::ExportFormat,
  CommandId::ExportSend,
  CommandId::ToggleInvisibility,
];

#[hotpath::measure]
pub fn default_keys_for(id: CommandId) -> &'static [&'static str] {
  command_spec(id)
    .map(|spec| spec.default_keys)
    .unwrap_or(&[])
}

#[hotpath::measure]
pub fn active_keys_for(id: CommandId) -> Vec<String> {
  crate::app_settings::load_keys_for_command(id)
}

#[hotpath::measure]
pub fn active_first_key_for(id: CommandId) -> Option<String> {
  crate::app_settings::load_first_key_for_command(id)
}

#[hotpath::measure]
pub fn label_for(id: CommandId) -> &'static str {
  command_spec(id)
    .map(|spec| spec.label)
    .unwrap_or("Unknown Command")
}

#[hotpath::measure]
pub fn context_for(id: CommandId) -> Option<&'static str> {
  command_spec(id).and_then(|spec| spec.context)
}
