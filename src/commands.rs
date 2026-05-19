mod keymap;

pub use keymap::{Keymap, KeymapEntry, register_default_keybindings, register_keymap};

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
  ToggleCite,
  ToggleUnderline,
  ToggleEmphasis,
  SetHighlightSpoken,
  ClearFormatting,
  ClearHighlight,
  Backspace,
  Delete,
  InsertNewline,
  InsertSoftLineBreak,
  NewDocument,
  OpenDocument,
  OpenDemoDocument,
  CloseDocument,
  ToggleRibbon,
  ScrollToParagraph,
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
  CommandSpec::new(CommandId::SelectWordLeft, "Select Word Left", EDITOR, &["ctrl-shift-left", "alt-shift-left"]),
  CommandSpec::new(CommandId::SelectWordRight, "Select Word Right", EDITOR, &["ctrl-shift-right", "alt-shift-right"]),
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
  CommandSpec::new(CommandId::ToggleCite, "Toggle Cite", EDITOR, &["f8"]),
  CommandSpec::new(CommandId::ToggleUnderline, "Toggle Underline", EDITOR, &["f9", "cmd-u", "ctrl-u"]),
  CommandSpec::new(CommandId::ToggleEmphasis, "Toggle Emphasis", EDITOR, &["f10", "cmd-b", "ctrl-b"]),
  CommandSpec::new(CommandId::SetHighlightSpoken, "Set Highlight: Spoken", EDITOR, &["f11"]),
  CommandSpec::new(CommandId::ClearFormatting, "Clear Formatting", EDITOR, &["f12"]),
  CommandSpec::new(CommandId::ClearHighlight, "Clear Highlight", EDITOR, &["ctrl-shift-h"]),
  CommandSpec::new(CommandId::Backspace, "Backspace", EDITOR, &["backspace"]),
  CommandSpec::new(CommandId::Delete, "Delete", EDITOR, &["delete"]),
  CommandSpec::new(CommandId::InsertNewline, "Insert Paragraph Break", EDITOR, &["enter"]),
  CommandSpec::new(CommandId::InsertSoftLineBreak, "Insert Soft Line Break", EDITOR, &["shift-enter"]),
  CommandSpec::new(CommandId::NewDocument, "New Document", APP, &[]),
  CommandSpec::new(CommandId::OpenDocument, "Open Document", APP, &[]),
  CommandSpec::new(CommandId::OpenDemoDocument, "Open Demo Document", APP, &[]),
  CommandSpec::new(CommandId::CloseDocument, "Close Document", APP, &[]),
  CommandSpec::new(CommandId::ToggleRibbon, "Toggle Ribbon", APP, &[]),
  CommandSpec::new(CommandId::ScrollToParagraph, "Scroll to Paragraph", APP, &[]),
];

pub fn command_spec(id: CommandId) -> Option<&'static CommandSpec> {
  COMMAND_SPECS.iter().find(|spec| spec.id == id)
}

pub fn default_keys_for(id: CommandId) -> &'static [&'static str] {
  command_spec(id).map(|spec| spec.default_keys).unwrap_or(&[])
}

pub fn label_for(id: CommandId) -> &'static str {
  command_spec(id).map(|spec| spec.label).unwrap_or("Unknown Command")
}
