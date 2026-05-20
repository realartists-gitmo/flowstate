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
  Highlight(HighlightStyle),
}

impl RichTextEditor {
  pub fn armed_inline_tool(&self) -> Option<ArmedInlineTool> {
    self.armed_inline_tool
  }

  /// Activate a Word-highlighter-like inline tool.
  ///
  /// If text is selected, the tool is applied immediately and is not armed. If
  /// the caret is empty, the tool is armed for future mouse selections and also
  /// updates pending caret styles so typed text follows the selected style.
  pub fn activate_inline_tool(
    &mut self,
    tool: ArmedInlineTool,
    cx: &mut Context<Self>,
  ) {
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

  fn apply_inline_tool_to_selection_with_current_behavior(
    &mut self,
    tool: ArmedInlineTool,
    cx: &mut Context<Self>,
  ) {
    match tool {
      ArmedInlineTool::Semantic(semantic) => self.toggle_semantic_style_for_selection(semantic, cx),
      ArmedInlineTool::Underline => self.toggle_underline(cx),
      ArmedInlineTool::Highlight(highlight) => self.set_highlight(highlight, cx),
    }
  }

  fn force_apply_inline_tool_to_selection(&mut self, tool: ArmedInlineTool, cx: &mut Context<Self>) {
    self.force_apply_inline_tool_to_current_target(tool, cx);
  }

  fn force_apply_inline_tool_to_current_target(&mut self, tool: ArmedInlineTool, cx: &mut Context<Self>) {
    if let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix }) = self.selected_block {
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
        for run in &mut paragraph.paragraph.runs {
          apply_inline_tool_to_styles(tool, &mut run.styles);
        }
        paragraph.paragraph.runs = merge_adjacent_runs(std::mem::take(&mut paragraph.paragraph.runs));
        paragraph.paragraph.version = paragraph.paragraph.version.wrapping_add(1);
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
      editor.after_text_mutation(cx);
    });
  }
}

fn apply_inline_tool_to_caret_styles(
  editor: &RichTextEditor,
  tool: ArmedInlineTool,
  styles: &mut RunStyles,
) {
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
    ArmedInlineTool::Highlight(highlight) => {
      styles.highlight = Some(highlight);
    },
  }
}

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
    ArmedInlineTool::Highlight(highlight) => {
      styles.highlight = Some(highlight);
    },
  }
}
