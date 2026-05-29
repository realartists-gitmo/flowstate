use gpui::Context;

use super::*;

/// Inline formatting tool that can stay armed while the user marks text.
///
/// This is separate from `pending_styles`: pending styles affect text typed at
/// the caret, while an armed tool is applied to future mouse selections.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArmedInlineTool {
  Semantic(RunSemanticStyle),
  Underline,
  Strikethrough,
  Highlight(HighlightStyle),
  ClearHighlight,
}

#[hotpath::measure_all]
impl RichTextEditor {
  pub fn armed_inline_tool(&self) -> Option<ArmedInlineTool> {
    self.armed_inline_tool
  }

  pub fn current_highlight_style(&self) -> HighlightStyle {
    self.current_highlight_style
  }

  pub fn current_highlight_choice(&self) -> Option<HighlightStyle> {
    self.current_highlight_choice
  }

  pub fn highlight_mode_active(&self) -> bool {
    matches!(
      self.armed_inline_tool,
      Some(ArmedInlineTool::Highlight(_) | ArmedInlineTool::ClearHighlight)
    )
  }

  /// Select the highlight picker mode.
  ///
  /// `Some(style)` means the picker is set to a real highlight color and the
  /// highlighter tool is armed. `None` means the picker is set to clear /
  /// transparent mode and any current selection highlight is removed.
  pub fn select_highlight_style(&mut self, highlight: Option<HighlightStyle>, cx: &mut Context<Self>) {
    match highlight {
      Some(highlight) => {
        self.current_highlight_style = highlight;
        self.current_highlight_choice = Some(highlight);

        if self.selection.is_caret() {
          self.armed_inline_tool = Some(ArmedInlineTool::Highlight(highlight));
          let mut styles = self.styles_at_caret();
          styles.highlight = Some(highlight);
          self.pending_styles = Some(styles);
          self.reset_caret_blink(cx);
        } else {
          self.armed_inline_tool = None;
          self.set_highlight_for_selection(Some(highlight), cx);
        }
      },
      None => {
        self.current_highlight_choice = None;
        self.armed_inline_tool = Some(ArmedInlineTool::ClearHighlight);

        if self.selection.is_caret() {
          let mut styles = self.styles_at_caret();
          styles.highlight = None;
          self.pending_styles = Some(styles);
          self.reset_caret_blink(cx);
        } else {
          self.set_highlight_for_selection(None, cx);
        }
      },
    }

    cx.notify();
  }

  pub fn toggle_highlight_mode(&mut self, cx: &mut Context<Self>) {
    if !self.selection.is_caret() {
      self.armed_inline_tool = None;
      self.set_highlight_for_selection(self.current_highlight_choice, cx);
      return;
    }

    if self.highlight_mode_active() {
      self.armed_inline_tool = None;
      self.pending_styles = None;
      self.reset_caret_blink(cx);
      cx.notify();
      return;
    }

    self.armed_inline_tool = Some(match self.current_highlight_choice {
      Some(highlight) => ArmedInlineTool::Highlight(highlight),
      None => ArmedInlineTool::ClearHighlight,
    });
    if self.selection.is_caret() {
      let mut styles = self.styles_at_caret();
      styles.highlight = self.current_highlight_choice;
      self.pending_styles = Some(styles);
      self.reset_caret_blink(cx);
    }
    cx.notify();
  }

  pub fn apply_current_highlight_to_selection(&mut self, cx: &mut Context<Self>) {
    self.set_highlight_for_selection(self.current_highlight_choice, cx);
  }

  /// Activate a Word-highlighter-like inline tool.
  ///
  /// If text is selected, the tool is applied immediately and is not armed. If
  /// the caret is empty, the tool is armed for future mouse selections and also
  /// updates pending caret styles so typed text follows the selected style.
  pub fn activate_inline_tool(&mut self, tool: ArmedInlineTool, cx: &mut Context<Self>) {
    if matches!(self.selected_block, Some(BlockSelection::TableCell { .. })) {
      self.armed_inline_tool = None;
      self.force_apply_inline_tool_to_current_target(tool, cx);
      return;
    }
    if self.selection.is_caret() {
      self.armed_inline_tool = Some(tool);
      self.apply_inline_tool_to_pending_styles(tool);
      self.reset_caret_blink(cx);
      cx.notify();
      return;
    }

    self.armed_inline_tool = None;
    self.apply_inline_tool_to_selection_with_current_behavior(tool, cx);
  }

  /// Toggle a ribbon-style inline tool on or off.
  ///
  /// When the caret has an armed tool, choosing the same tool again should
  /// disarm it and undo the pending caret style. With selected text this keeps
  /// the existing document-style toggle behavior.
  pub fn toggle_inline_tool(&mut self, tool: ArmedInlineTool, cx: &mut Context<Self>) {
    if self.selection.is_caret() && self.armed_inline_tool == Some(tool) {
      self.armed_inline_tool = None;
      self.apply_inline_tool_to_pending_styles(tool);
      self.reset_caret_blink(cx);
      cx.notify();
      return;
    }

    self.activate_inline_tool(tool, cx);
  }

  pub fn clear_matching_armed_inline_tool(&mut self, tool: ArmedInlineTool, cx: &mut Context<Self>) -> bool {
    if self.selection.is_caret() && self.armed_inline_tool == Some(tool) {
      self.armed_inline_tool = None;
      self.apply_inline_tool_to_pending_styles(tool);
      self.reset_caret_blink(cx);
      cx.notify();
      return true;
    }

    false
  }

  pub fn clear_armed_inline_tool(&mut self, cx: &mut Context<Self>) {
    if self.armed_inline_tool.is_some() {
      self.armed_inline_tool = None;
      cx.notify();
    }
  }

  /// Apply the active tool after a mouse selection finishes.
  pub(super) fn apply_armed_inline_tool_to_selection(&mut self, cx: &mut Context<Self>) {
    let Some(tool) = self.armed_inline_tool else {
      return;
    };
    if self.selection.is_caret() {
      return;
    }
    self.force_apply_inline_tool_to_selection(tool, cx);
  }

  fn apply_inline_tool_to_pending_styles(&mut self, tool: ArmedInlineTool) {
    let mut styles = self.styles_at_caret();
    apply_inline_tool_to_caret_styles(self, tool, &mut styles);
    self.pending_styles = Some(styles);
  }

  fn apply_inline_tool_to_selection_with_current_behavior(&mut self, tool: ArmedInlineTool, cx: &mut Context<Self>) {
    match tool {
      ArmedInlineTool::Semantic(semantic) => self.toggle_semantic_style_for_selection(semantic, cx),
      ArmedInlineTool::Underline => self.toggle_underline(cx),
      ArmedInlineTool::Strikethrough => self.toggle_strikethrough(cx),
      ArmedInlineTool::Highlight(highlight) => self.set_highlight(highlight, cx),
      ArmedInlineTool::ClearHighlight => self.set_highlight_for_selection(None, cx),
    }
  }

  fn force_apply_inline_tool_to_selection(&mut self, tool: ArmedInlineTool, cx: &mut Context<Self>) {
    self.force_apply_inline_tool_to_current_target(tool, cx);
  }

  fn force_apply_inline_tool_to_current_target(&mut self, tool: ArmedInlineTool, cx: &mut Context<Self>) {
    if let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix }) = self.selected_block {
      let Some(selection_range) = self.table_cell_selection_range() else {
        return;
      };
      self.edit_table_cell_paragraph(block_ix, row_ix, cell_ix, cx, |paragraph| {
        if paragraph.text.is_empty() {
          return;
        }
        if paragraph.paragraph.runs.is_empty() {
          paragraph.paragraph.runs.push(TextRun {
            len: paragraph.text.len(),
            styles: RunStyles::default(),
          });
        }
        mutate_table_cell_runs_in_range(paragraph, selection_range.clone(), |styles| {
          apply_inline_tool_to_styles(tool, styles);
        });
      });
      return;
    }
    if self.selection.is_caret() {
      return;
    }
    let range = self.selection.normalized();
    self.apply_document_edit(cx, |editor, cx| {
      mutate_runs_in_range(&mut editor.document, range, |styles| {
        apply_inline_tool_to_styles(tool, styles);
      });
      editor.after_formatting_mutation(cx);
    });
  }
}

#[hotpath::measure]
fn apply_inline_tool_to_caret_styles(editor: &RichTextEditor, tool: ArmedInlineTool, styles: &mut RunStyles) {
  match tool {
    ArmedInlineTool::Semantic(semantic) => {
      styles.semantic = if styles.semantic == semantic {
        RunSemanticStyle::Plain
      } else {
        semantic
      };
      if styles.semantic != RunSemanticStyle::Underline {
        styles.direct_underline = false;
      }
    },
    ArmedInlineTool::Underline => {
      let paragraph_style = editor.document.paragraphs[editor.selection.head.paragraph].style;
      let direct = matches!(paragraph_style, ParagraphStyle::Tag | ParagraphStyle::Analytic);
      if direct {
        styles.direct_underline = !styles.direct_underline;
      } else if styles.semantic == RunSemanticStyle::Underline {
        styles.semantic = RunSemanticStyle::Plain;
      } else {
        styles.semantic = RunSemanticStyle::Underline;
        styles.direct_underline = false;
      }
    },
    ArmedInlineTool::Strikethrough => {
      styles.strikethrough = !styles.strikethrough;
    },
    ArmedInlineTool::Highlight(highlight) => {
      styles.highlight = if styles.highlight == Some(highlight) { None } else { Some(highlight) };
    },
    ArmedInlineTool::ClearHighlight => {
      styles.highlight = None;
    },
  }
}

#[hotpath::measure]
fn apply_inline_tool_to_styles(tool: ArmedInlineTool, styles: &mut RunStyles) {
  match tool {
    ArmedInlineTool::Semantic(semantic) => {
      styles.semantic = semantic;
      if semantic != RunSemanticStyle::Underline {
        styles.direct_underline = false;
      }
    },
    ArmedInlineTool::Underline => {
      styles.semantic = RunSemanticStyle::Underline;
      styles.direct_underline = false;
    },
    ArmedInlineTool::Strikethrough => {
      styles.strikethrough = true;
    },
    ArmedInlineTool::Highlight(highlight) => {
      styles.highlight = Some(highlight);
    },
    ArmedInlineTool::ClearHighlight => {
      styles.highlight = None;
    },
  }
}
