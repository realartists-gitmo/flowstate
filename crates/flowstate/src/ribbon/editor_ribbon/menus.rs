#[hotpath::measure]
fn modern_highlight_menu(
  command: &RibbonCommand,
  editor: Entity<RichTextEditor>,
  document_theme: &DocumentTheme,
  metrics: RibbonLayoutMetrics,
  cx: &mut Context<EditorRibbon>,
) -> AnyElement {
  let accent = command.accent.unwrap_or(RibbonAccent::Gray);
  let selected_highlight = match command.id {
    RibbonCommandId::ToggleHighlightMode(style) => style,
    _ => None,
  };
  let mode_active = command.selected;
  let document_theme = document_theme.clone();
  let command_color = ribbon_command_color(command, cx);

  let chip_height = metrics.chip_height;

  div()
    .flex()
    .flex_row()
    .items_center()
    .gap_0()
    .child(
      div().relative().h(chip_height).child(
        DropdownButton::new("modern-ribbon-highlight-dropdown")
          .with_size(Size::Size(chip_height))
          .compact()
          .outline()
          .button(
            Button::new(("modern-ribbon-highlight-toggle", 0_u64))
              .compact()
              .ghost()
              .h(chip_height)
              .px(metrics.chip_padding_x)
              .when(mode_active, |this| {
                this
                  .bg(command_color.opacity(0.18))
                  .border_color(command_color)
                  .text_color(command_color)
              })
              .tooltip_with_action("Highlight mode", &ApplyHighlightToSelection, Some("RichTextEditor"))
              .child(match accent {
                RibbonAccent::Transparent => transparent_accent_bar(cx),
                _ => accent_bar(accent_color(accent, cx), cx),
              })
              .child(
                Icon::default()
                  .path("icons/highlighter.svg")
                  .xsmall()
                  .text_color(command_color),
              )
              .when_some(shortcut_for(CommandId::ApplyHighlightToSelection), |this, shortcut| {
                this.child(keycap(shortcut, cx))
              })
              .on_click({
                let editor = editor.clone();
                move |_, _, cx| {
                  editor.update(cx, |editor, cx| {
                    editor.toggle_highlight_mode(cx);
                  });
                }
              }),
          )
          .dropdown_menu(move |menu, _, _| {
            let menu = menu.min_w(px(180.0)).max_w(px(220.0));

            let menu = HIGHLIGHT_STYLE_SPECS.iter().fold(menu, |menu, spec| {
              let style = spec.style;
              let label = spec.label;
              let editor = editor.clone();
              let color = highlight_color(style, &document_theme);

              menu.item(
                PopupMenuItem::element(move |_, _| {
                  div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(highlight_menu_swatch(color))
                    .child(label)
                })
                .checked(selected_highlight == Some(style))
                .on_click(move |_, _, cx| {
                  editor.update(cx, |editor, cx| {
                    editor.select_highlight_style(Some(style), cx);
                  });
                }),
              )
            });

            let editor = editor.clone();

            menu.item(
              PopupMenuItem::new("Clear highlight")
                .checked(selected_highlight.is_none())
                .on_click(move |_, _, cx| {
                  editor.update(cx, |editor, cx| {
                    editor.select_highlight_style(None, cx);
                  });
                }),
            )
          }),
      ),
    )
    .into_any_element()
}

#[hotpath::measure]
fn modern_condense_menu(
  command: &RibbonCommand,
  editor: Entity<RichTextEditor>,
  metrics: RibbonLayoutMetrics,
  options: ModernRibbonOptions,
  cx: &mut Context<EditorRibbon>,
) -> AnyElement {
  let chip_height = metrics.chip_height;
  let label = RibbonLabel::for_command(command);
  let command_color = ribbon_command_color(command, cx);
  let shortcut = command.shortcut.clone();
  let condense_action = command
    .command_id
    .and_then(action_for_command)
    .map(|action| (action, command.command_id.and_then(context_for)));
  let has_condense_action = condense_action.is_some();

  DropdownButton::new("modern-ribbon-condense-dropdown")
    .with_size(Size::Size(chip_height))
    .compact()
    .outline()
    .button(
      Button::new("modern-ribbon-condense-toggle")
        .compact()
        .ghost()
        .h(chip_height)
        .px(metrics.chip_padding_x)
        .when_some(condense_action, |this, (action_box, ctx)| {
          this.tooltip_with_action(command.label, &*action_box, ctx)
        })
        .when(!has_condense_action, |this| this.tooltip(command.label))
        .when_some(label.icon_path, |this, path| {
          this.child(
            Icon::default()
              .path(path)
              .xsmall()
              .text_color(command_color),
          )
        })
        .when(!label.prefers_icon(), |this| {
          this.child(
            div()
              .flex_none()
              .text_size(metrics.chip_text_size)
              .line_height(relative(1.0))
              .whitespace_nowrap()
              .text_ellipsis()
              .text_color(command_color)
              .child(label.text),
          )
        })
        .when(show_shortcut(options), |this| {
          this.when_some(shortcut, |this, shortcut| this.child(keycap(shortcut, cx)))
        })
        .on_click({
          let editor = editor.clone();
          move |_, window, cx| {
            condense_editor_selection(editor.clone(), ' ', window, cx);
          }
        }),
    )
    .dropdown_menu(move |menu, _, _| {
      let editor_for_plain_condense = editor.clone();
      let editor_for_pilcrow_condense = editor.clone();
      let editor_for_uncondense = editor.clone();
      menu
        .min_w(px(190.0))
        .item(PopupMenuItem::new("Condense").on_click(move |_, window, cx| {
          condense_editor_selection(editor_for_plain_condense.clone(), ' ', window, cx);
        }))
        .item(PopupMenuItem::new("Condense with pilcrows").on_click(move |_, window, cx| {
          condense_editor_selection(editor_for_pilcrow_condense.clone(), CONDENSE_PILCROW_MARKER, window, cx);
        }))
        .item(PopupMenuItem::new("Uncondense pilcrows").on_click(move |_, window, cx| {
          uncondense_editor_selection(editor_for_uncondense.clone(), window, cx);
        }))
    })
    .into_any_element()
}

#[hotpath::measure]
fn modern_condensed_menu(
  command: &RibbonCommand,
  editor: Entity<RichTextEditor>,
  metrics: RibbonLayoutMetrics,
  options: ModernRibbonOptions,
  cx: &mut Context<EditorRibbon>,
) -> AnyElement {
  let mode_active = command.selected;
  let checked = command.selected;
  let chip_height = metrics.chip_height;
  let label = RibbonLabel::for_command(command);
  let command_color = ribbon_command_color(command, cx);
  let shortcut = command.shortcut.clone();

  DropdownButton::new("modern-ribbon-condensed-dropdown")
    .with_size(Size::Size(chip_height))
    .compact()
    .outline()
    .button(
      Button::new("modern-ribbon-condensed-toggle")
        .compact()
        .ghost()
        .h(chip_height)
        .px(metrics.chip_padding_x)
        .when(mode_active, |this| {
          this
            .bg(command_color.opacity(0.18))
            .border_color(command_color)
            .text_color(command_color)
        })
        .tooltip("Shrink")
        .when_some(label.icon_path, |this, path| {
          this.child(
            Icon::default()
              .path(path)
              .xsmall()
              .text_color(command_color),
          )
        })
        .when(!label.prefers_icon(), |this| {
          this.child(
            div()
              .flex_none()
              .text_size(metrics.chip_text_size)
              .line_height(relative(1.0))
              .whitespace_nowrap()
              .text_ellipsis()
              .text_color(command_color)
              .child(label.text),
          )
        })
        .when(show_shortcut(options), |this| {
          this.when_some(shortcut, |this, shortcut| this.child(keycap(shortcut, cx)))
        })
        .on_click({
          let editor = editor.clone();
          move |_, window, cx| {
            apply_shrink_editor_selection(editor.clone(), flowstate_document::SEMANTIC_CONDENSED, window, cx);
          }
        }),
    )
    .dropdown_menu(move |menu, _, _| {
      let editor_for_condensed = editor.clone();
      let editor_for_ultra = editor.clone();
      menu
        .min_w(px(190.0))
        .item(
          PopupMenuItem::new("Shrink")
            .checked(checked)
            .on_click(move |_, window, cx| {
              apply_shrink_editor_selection(editor_for_condensed.clone(), flowstate_document::SEMANTIC_CONDENSED, window, cx);
            }),
        )
        .item(PopupMenuItem::new("Ultra shrink").on_click(move |_, window, cx| {
          apply_shrink_editor_selection(editor_for_ultra.clone(), flowstate_document::SEMANTIC_ULTRACONDENSED, window, cx);
        }))
    })
    .into_any_element()
}

pub(crate) const CONDENSE_PILCROW_MARKER: char = '\u{f8ff}';
const CARD_SECTION_SLOTS: &[u8] = flowstate_document::CARD_BOUNDARY_STYLE_SLOTS;

fn editor_has_selected_text_or_focused_caret(editor: &RichTextEditor, window: &Window, cx: &App) -> bool {
  !editor.selection().is_caret() || editor.focus_handle(cx).is_focused(window)
}

pub(crate) fn condense_editor_selection(editor: Entity<RichTextEditor>, separator: char, window: &Window, cx: &mut App) {
  editor.update(cx, |editor, cx| {
    if !editor_has_selected_text_or_focused_caret(editor, window, cx) {
      return;
    }
    let use_card_fallback = editor.selection().is_caret();
    let Some(fragment) = editor.fragment_at_selection_or_enclosing_section(CARD_SECTION_SLOTS) else {
      return;
    };
    let paragraphs = if use_card_fallback {
      condense_card_fragment_paragraphs(fragment.paragraphs, separator)
    } else {
      condense_fragment_paragraphs(fragment.paragraphs, separator)
    };
    editor.replace_selection_or_enclosing_section_with_paragraphs(paragraphs, CARD_SECTION_SLOTS, cx);
  });
}

pub(crate) fn uncondense_editor_selection(editor: Entity<RichTextEditor>, window: &Window, cx: &mut App) {
  editor.update(cx, |editor, cx| {
    if !editor_has_selected_text_or_focused_caret(editor, window, cx) {
      return;
    }
    let use_card_fallback = editor.selection().is_caret();
    let Some(fragment) = editor.fragment_at_selection_or_enclosing_section(CARD_SECTION_SLOTS) else {
      return;
    };
    let paragraphs = if use_card_fallback {
      uncondense_card_fragment_paragraphs(fragment.paragraphs)
    } else {
      uncondense_fragment_paragraphs(fragment.paragraphs)
    };
    editor.replace_selection_or_enclosing_section_with_paragraphs(paragraphs, CARD_SECTION_SLOTS, cx);
  });
}

fn apply_shrink_editor_selection(editor: Entity<RichTextEditor>, semantic: RunSemanticStyle, window: &Window, cx: &mut App) {
  editor.update(cx, |editor, cx| {
    if !editor_has_selected_text_or_focused_caret(editor, window, cx) {
      return;
    }
    if !editor.selection().is_caret() {
      editor.toggle_inline_tool(ArmedInlineTool::Semantic(semantic), cx);
      return;
    }
    let Some(fragment) = editor.fragment_at_selection_or_enclosing_section(CARD_SECTION_SLOTS) else {
      return;
    };
    let paragraphs = toggle_card_semantic_paragraphs(fragment.paragraphs, semantic);
    editor.replace_selection_or_enclosing_section_with_paragraphs(paragraphs, CARD_SECTION_SLOTS, cx);
  });
}

fn condense_fragment_paragraphs(
  paragraphs: Vec<flowstate_document::InputParagraph>,
  separator: char,
) -> Vec<flowstate_document::InputParagraph> {
  condense_paragraph_group(paragraphs, separator)
    .map(|paragraph| vec![paragraph, empty_input_paragraph()])
    .unwrap_or_default()
}

fn condense_card_fragment_paragraphs(
  paragraphs: Vec<flowstate_document::InputParagraph>,
  separator: char,
) -> Vec<flowstate_document::InputParagraph> {
  transform_card_paragraph_groups(paragraphs, |group| {
    condense_paragraph_group(group, separator)
      .into_iter()
      .collect()
  })
}

fn condense_paragraph_group(paragraphs: Vec<flowstate_document::InputParagraph>, separator: char) -> Option<flowstate_document::InputParagraph> {
  let mut runs = Vec::new();
  for paragraph in paragraphs {
    let mut paragraph_runs = paragraph
      .runs
      .into_iter()
      .filter(|run| !run.text.is_empty())
      .peekable();
    if paragraph_runs.peek().is_none() {
      continue;
    }
    if !runs.is_empty() {
      runs.push(flowstate_document::InputRun {
        text: separator.to_string(),
        styles: flowstate_document::RunStyles::default(),
      });
    }
    runs.extend(paragraph_runs);
  }
  (!runs.is_empty()).then_some(flowstate_document::InputParagraph {
    style: flowstate_document::ParagraphStyle::Normal,
    runs,
  })
}

fn uncondense_fragment_paragraphs(paragraphs: Vec<flowstate_document::InputParagraph>) -> Vec<flowstate_document::InputParagraph> {
  let mut output = uncondense_paragraph_group(paragraphs);
  output.push(empty_input_paragraph());
  output
}

fn uncondense_card_fragment_paragraphs(paragraphs: Vec<flowstate_document::InputParagraph>) -> Vec<flowstate_document::InputParagraph> {
  transform_card_paragraph_groups(paragraphs, uncondense_paragraph_group)
}

fn uncondense_paragraph_group(paragraphs: Vec<flowstate_document::InputParagraph>) -> Vec<flowstate_document::InputParagraph> {
  let mut output = vec![empty_input_paragraph()];
  for paragraph in paragraphs {
    for run in paragraph.runs {
      let mut remainder = run.text.as_str();
      while let Some(marker_ix) = remainder.find(CONDENSE_PILCROW_MARKER) {
        let before = &remainder[..marker_ix];
        if !before.is_empty() {
          output
            .last_mut()
            .expect("output has current paragraph")
            .runs
            .push(flowstate_document::InputRun {
              text: before.to_string(),
              styles: run.styles,
            });
        }
        output.push(empty_input_paragraph());
        remainder = &remainder[marker_ix + CONDENSE_PILCROW_MARKER.len_utf8()..];
      }
      if !remainder.is_empty() {
        output
          .last_mut()
          .expect("output has current paragraph")
          .runs
          .push(flowstate_document::InputRun {
            text: remainder.to_string(),
            styles: run.styles,
          });
      }
    }
  }
  output
}

fn transform_card_paragraph_groups(
  paragraphs: Vec<flowstate_document::InputParagraph>,
  mut transform: impl FnMut(Vec<flowstate_document::InputParagraph>) -> Vec<flowstate_document::InputParagraph>,
) -> Vec<flowstate_document::InputParagraph> {
  let mut output = Vec::with_capacity(paragraphs.len());
  let mut group = Vec::new();
  let mut transformed_any = false;
  for paragraph in paragraphs {
    if card_paragraph_excluded_from_condense(&paragraph) {
      if !group.is_empty() {
        let transformed = transform(std::mem::take(&mut group));
        transformed_any |= !transformed.is_empty();
        output.extend(transformed);
      }
      output.push(paragraph);
    } else {
      group.push(paragraph);
    }
  }
  if !group.is_empty() {
    let transformed = transform(group);
    transformed_any |= !transformed.is_empty();
    output.extend(transformed);
  }
  if transformed_any { output } else { Vec::new() }
}

fn toggle_card_semantic_paragraphs(
  mut paragraphs: Vec<flowstate_document::InputParagraph>,
  semantic: RunSemanticStyle,
) -> Vec<flowstate_document::InputParagraph> {
  let mut eligible_run_count = 0usize;
  let mut all_eligible_have_semantic = true;
  for paragraph in &paragraphs {
    if card_paragraph_excluded_from_condense(paragraph) {
      continue;
    }
    for run in paragraph.runs.iter().filter(|run| !run.text.is_empty()) {
      eligible_run_count += 1;
      all_eligible_have_semantic &= run.styles.semantic == semantic;
    }
  }
  if eligible_run_count == 0 {
    return Vec::new();
  }
  let target = if all_eligible_have_semantic {
    flowstate_document::RunSemanticStyle::Plain
  } else {
    semantic
  };
  for paragraph in &mut paragraphs {
    if card_paragraph_excluded_from_condense(paragraph) {
      continue;
    }
    for run in paragraph.runs.iter_mut().filter(|run| !run.text.is_empty()) {
      run.styles.semantic = target;
      if target != flowstate_document::SEMANTIC_UNDERLINE {
        run.styles.direct_underline = false;
      }
    }
  }
  paragraphs
}

fn card_paragraph_excluded_from_condense(paragraph: &flowstate_document::InputParagraph) -> bool {
  paragraph.style == flowstate_document::PARAGRAPH_TAG
    || paragraph
      .runs
      .iter()
      .any(|run| run.styles.semantic == flowstate_document::SEMANTIC_CITE)
}

fn empty_input_paragraph() -> flowstate_document::InputParagraph {
  flowstate_document::InputParagraph {
    style: flowstate_document::ParagraphStyle::Normal,
    runs: Vec::new(),
  }
}

#[hotpath::measure]
#[hotpath::measure]
fn send_format_from_ribbon(
  editor: Entity<RichTextEditor>,
  format: DocumentExportFormat,
  workspace: Option<WeakEntity<Workspace>>,
  cx: &mut App,
) {
  let task = editor.update(cx, |editor, cx| editor.send_document(format, cx));
  cx.spawn(async move |cx| {
    let result = task.await;
    let Some(workspace) = workspace else {
      if let Err(error) = result {
        tracing::error!("send export failed with no workspace to report to: {error}");
      }
      return;
    };
    let _ = workspace.update(cx, |workspace, cx| match result {
      // SB-S3: exports announce themselves in the activity zone (Law 2 both
      // ways — completion is visible, failure is loud).
      Ok(path) => workspace.report_activity(format!("Sent {}", path.display()), cx),
      Err(error) => workspace.report_failure(format!("Send failed: {error}"), None, cx),
    });
  })
  .detach();
}

#[hotpath::measure]
fn export_format_from_ribbon(
  editor: Entity<RichTextEditor>,
  format: DocumentExportFormat,
  workspace: Option<WeakEntity<Workspace>>,
  cx: &mut App,
) {
  let task = editor.update(cx, |editor, cx| editor.export_document_format(format, cx));
  cx.spawn(async move |cx| {
    let result = task.await;
    let Some(workspace) = workspace else {
      if let Err(error) = result {
        tracing::error!("format export failed with no workspace to report to: {error}");
      }
      return;
    };
    let _ = workspace.update(cx, |workspace, cx| match result {
      Ok(path) => workspace.report_activity(format!("Exported {}", path.display()), cx),
      Err(error) => workspace.report_failure(format!("Export failed: {error}"), None, cx),
    });
  })
  .detach();
}

#[hotpath::measure]
fn modern_undo_button(
  command: &RibbonCommand,
  editor: Entity<RichTextEditor>,
  metrics: RibbonLayoutMetrics,
  cx: &mut Context<EditorRibbon>,
) -> AnyElement {
  let enabled = editor.read(cx).can_undo();
  let undo_action = action_for_command(CommandId::Undo).map(|a| (a, context_for(CommandId::Undo)));
  modern_icon_chip(
    IconName::Undo,
    "Undo",
    undo_action,
    command,
    editor,
    metrics,
    cx,
    enabled,
    |editor, cx| editor.undo(cx),
  )
}

#[hotpath::measure]
fn modern_redo_button(
  command: &RibbonCommand,
  editor: Entity<RichTextEditor>,
  metrics: RibbonLayoutMetrics,
  cx: &mut Context<EditorRibbon>,
) -> AnyElement {
  let enabled = editor.read(cx).can_redo();
  let redo_action = action_for_command(CommandId::Redo).map(|a| (a, context_for(CommandId::Redo)));
  modern_icon_chip(
    IconName::Redo,
    "Redo",
    redo_action,
    command,
    editor,
    metrics,
    cx,
    enabled,
    |editor, cx| editor.redo(cx),
  )
}

fn modern_icon_chip(
  icon: IconName,
  tooltip: &'static str,
  action_with_ctx: Option<(Box<dyn Action>, Option<&'static str>)>,
  command: &RibbonCommand,
  editor: Entity<RichTextEditor>,
  metrics: RibbonLayoutMetrics,
  cx: &mut Context<EditorRibbon>,
  enabled: bool,
  action: impl Fn(&mut RichTextEditor, &mut Context<RichTextEditor>) + 'static,
) -> AnyElement {
  let command_color = ribbon_command_color(command, cx);
  let icon_color = if enabled {
    command_color
  } else {
    cx.theme().muted_foreground.opacity(0.5)
  };
  let has_tooltip_action = action_with_ctx.is_some();
  Button::new(("modern-ribbon-command", ribbon_command_key(command.id)))
    .xsmall()
    .compact()
    .outline()
    .h(metrics.chip_height)
    .w(metrics.chip_height)
    .px(metrics.chip_padding_x)
    .icon(Icon::new(icon).xsmall().text_color(icon_color))
    .when_some(action_with_ctx, |this, (action_box, ctx)| {
      this.tooltip_with_action(tooltip, &*action_box, ctx)
    })
    .when(!has_tooltip_action, |this| this.tooltip(tooltip))
    .disabled(!enabled)
    .on_click(move |_, _, cx| {
      editor.update(cx, |editor, cx| action(editor, cx));
    })
    .into_any_element()
}

#[hotpath::measure]
fn modern_export_format(
  command: &RibbonCommand,
  editor: Entity<RichTextEditor>,
  metrics: RibbonLayoutMetrics,
  workspace: Option<WeakEntity<Workspace>>,
  cx: &mut Context<EditorRibbon>,
) -> AnyElement {
  let chip_height = metrics.chip_height;
  let command_color = ribbon_command_color(command, cx);
  let label = RibbonLabel::for_command(command);
  let format_created = editor
    .read(cx)
    .format_export_created_since_last_saved_edit();
  DropdownButton::new("modern-ribbon-format-dropdown")
    .with_size(Size::Size(chip_height))
    .compact()
    .outline()
    .when(format_created, |this| this.dropdown_icon(IconName::Check, Some(cx.theme().success)))
    .button(
      Button::new("modern-ribbon-export-format")
        .compact()
        .ghost()
        .h(chip_height)
        .px(metrics.chip_padding_x)
        .tooltip("Export a copy of this document (.docx)")
        .when_some(label.icon_path, |this, path| {
          this.child(
            Icon::default()
              .path(path)
              .xsmall()
              .text_color(command_color),
          )
        })
        .on_click({
          let editor = editor.clone();
          let workspace = workspace.clone();
          move |_, _, cx| {
            export_format_from_ribbon(editor.clone(), DocumentExportFormat::Docx, workspace.clone(), cx);
          }
        }),
    )
    .dropdown_menu(move |menu, _, _| {
      let docx_editor = editor.clone();
      let docx_workspace = workspace.clone();
      let pdf_workspace = workspace.clone();
      let pdf_editor = editor.clone();
      menu
        .min_w(px(100.0))
        .item(
          PopupMenuItem::element(|_, _| {
            h_flex()
              .flex_1()
              .justify_end()
              .child(".docx")
              .into_any_element()
          })
          .icon(Icon::default().path("icons/docx.svg").small())
          .on_click(move |_, _, cx| {
            export_format_from_ribbon(docx_editor.clone(), DocumentExportFormat::Docx, docx_workspace.clone(), cx);
          }),
        )
        .item(
          PopupMenuItem::element(|_, _| {
            h_flex()
              .flex_1()
              .justify_end()
              .child(".pdf")
              .into_any_element()
          })
          .icon(Icon::default().path("icons/pdf.svg").small())
          .on_click(move |_, _, cx| {
            export_format_from_ribbon(pdf_editor.clone(), DocumentExportFormat::Pdf, pdf_workspace.clone(), cx);
          }),
        )
    })
    .into_any_element()
}

#[hotpath::measure]
fn modern_export_send(
  command: &RibbonCommand,
  editor: Entity<RichTextEditor>,
  metrics: RibbonLayoutMetrics,
  workspace: Option<WeakEntity<Workspace>>,
  cx: &mut Context<EditorRibbon>,
) -> AnyElement {
  let chip_height = metrics.chip_height;
  let command_color = ribbon_command_color(command, cx);
  let label = RibbonLabel::for_command(command);
  let send_created = editor
    .read(cx)
    .send_document_created_since_last_saved_edit();
  DropdownButton::new("modern-ribbon-send-dropdown")
    .with_size(Size::Size(chip_height))
    .compact()
    .outline()
    .when(send_created, |this| this.dropdown_icon(IconName::Check, Some(cx.theme().success)))
    .button(
      Button::new("modern-ribbon-export-send")
        .compact()
        .ghost()
        .h(chip_height)
        .px(metrics.chip_padding_x)
        .tooltip("Send a copy to the send directory (.db8)")
        .when_some(label.icon_path, |this, path| {
          this.child(
            Icon::default()
              .path(path)
              .xsmall()
              .text_color(command_color),
          )
        })
        .on_click({
          let editor = editor.clone();
          let workspace = workspace.clone();
          move |_, _, cx| {
            send_format_from_ribbon(
              editor.clone(),
              DocumentExportFormat::NativeWithExtension(flowstate_document::FLOWSTATE_EXTENSION),
              workspace.clone(),
              cx,
            );
          }
        }),
    )
    .dropdown_menu(move |menu, _, _| {
      let db8_editor = editor.clone();
      let docx_editor = editor.clone();
      let pdf_editor = editor.clone();
      let db8_workspace = workspace.clone();
      let docx_workspace = workspace.clone();
      let pdf_workspace = workspace.clone();
      menu
        .min_w(px(100.0))
        .item(
          PopupMenuItem::element(|_, _| {
            h_flex()
              .flex_1()
              .justify_end()
              .child(".db8")
              .into_any_element()
          })
          .on_click(move |_, _, cx| {
            send_format_from_ribbon(
              db8_editor.clone(),
              DocumentExportFormat::NativeWithExtension(flowstate_document::FLOWSTATE_EXTENSION),
              db8_workspace.clone(),
              cx,
            );
          }),
        )
        .item(
          PopupMenuItem::element(|_, _| {
            h_flex()
              .flex_1()
              .justify_end()
              .child(".docx")
              .into_any_element()
          })
          .icon(Icon::default().path("icons/docx.svg").small())
          .on_click(move |_, _, cx| {
            send_format_from_ribbon(docx_editor.clone(), DocumentExportFormat::Docx, docx_workspace.clone(), cx);
          }),
        )
        .item(
          PopupMenuItem::element(|_, _| {
            h_flex()
              .flex_1()
              .justify_end()
              .child(".pdf")
              .into_any_element()
          })
          .icon(Icon::default().path("icons/pdf.svg").small())
          .on_click(move |_, _, cx| {
            send_format_from_ribbon(pdf_editor.clone(), DocumentExportFormat::Pdf, pdf_workspace.clone(), cx);
          }),
        )
    })
    .into_any_element()
}

#[hotpath::measure]
fn modern_speech_send_menu(
  command: &RibbonCommand,
  _editor: Entity<RichTextEditor>,
  metrics: RibbonLayoutMetrics,
  workspace: Option<WeakEntity<Workspace>>,
  _panel_id: Option<Uuid>,
  cx: &mut Context<EditorRibbon>,
) -> AnyElement {
  let chip_height = metrics.chip_height;
  let command_color = ribbon_command_color(command, cx);
  let label = RibbonLabel::for_command(command);
  let shortcut = shortcut_for(CommandId::SendToSpeechDocument);

  DropdownButton::new("modern-ribbon-speech-send-dropdown")
    .with_size(Size::Size(chip_height))
    .compact()
    .outline()
    .button(
      Button::new("modern-ribbon-speech-send-toggle")
        .compact()
        .ghost()
        .h(chip_height)
        .px(metrics.chip_padding_x)
        // CT-S1: the full-card law, stated where the verb lives — a send in
        // invisibility mode carries the COMPLETE card (hidden text included),
        // never the filtered view. Evidence integrity over WYSIWYG.
        .tooltip("Send to speech \u{2014} sends the full card, including text hidden by invisibility mode")
        .when_some(label.icon_path, |this, path| {
          this.child(
            Icon::default()
              .path(path)
              .xsmall()
              .text_color(command_color),
          )
        })
        .when(!label.prefers_icon(), |this| {
          this.child(
            div()
              .flex_none()
              .text_size(metrics.chip_text_size)
              .line_height(relative(1.0))
              .whitespace_nowrap()
              .text_ellipsis()
              .text_color(command_color)
              .child(label.text),
          )
        })
        .when_some(shortcut, |this, shortcut| this.child(keycap(shortcut, cx)))
        .on_click({
          let workspace = workspace.clone();
          move |_, window, cx| {
            if let Some(workspace) = workspace.clone()
              && let Err(err) = workspace.update(cx, |workspace, cx| workspace.send_selection_to_speech_document(window, cx))
            {
              eprintln!("failed to send selection to speech document: {err:?}");
            }
          }
        }),
    )
    .dropdown_menu(move |menu, _, cx| {
      let send_workspace = workspace.clone();
      let send_end_workspace = workspace.clone();
      let speech_shortcut = shortcut_for(CommandId::SendToSpeechDocument);
      let speech_end_shortcut = shortcut_for(CommandId::SendToSpeechDocumentEnd);
      let muted = cx.theme().muted_foreground;
      menu
        .min_w(px(160.0))
        .item(
          PopupMenuItem::element({
            let speech_shortcut = speech_shortcut.clone();
            move |_, _| {
              h_flex()
                .flex_1()
                .justify_between()
                .gap_3()
                .child("Send to speech")
                .when_some(speech_shortcut.clone(), |this, s| {
                  this.child(
                    div()
                      .flex_none()
                      .whitespace_nowrap()
                      .text_size(px(10.0))
                      .line_height(relative(1.0))
                      .text_color(muted)
                      .child(s),
                  )
                })
                .into_any_element()
            }
          })
          .on_click(move |_, window, cx| {
            if let Some(workspace) = send_workspace.clone()
              && let Err(err) = workspace.update(cx, |workspace, cx| workspace.send_selection_to_speech_document(window, cx))
            {
              eprintln!("failed to send selection to speech document: {err:?}");
            }
          }),
        )
        .item(
          PopupMenuItem::element({
            let speech_end_shortcut = speech_end_shortcut.clone();
            move |_, _| {
              h_flex()
                .flex_1()
                .justify_between()
                .gap_3()
                .child("Send to speech end")
                .when_some(speech_end_shortcut.clone(), |this, s| {
                  this.child(
                    div()
                      .flex_none()
                      .whitespace_nowrap()
                      .text_size(px(10.0))
                      .line_height(relative(1.0))
                      .text_color(muted)
                      .child(s),
                  )
                })
                .into_any_element()
            }
          })
          .on_click(move |_, window, cx| {
            if let Some(workspace) = send_end_workspace.clone()
              && let Err(err) = workspace.update(cx, |workspace, cx| workspace.send_selection_to_speech_document_end(window, cx))
            {
              eprintln!("failed to send selection to speech document end: {err:?}");
            }
          }),
        )
    })
    .into_any_element()
}

#[hotpath::measure]
fn modern_invisibility_toggle(
  command: &RibbonCommand,
  editor: Entity<RichTextEditor>,
  metrics: RibbonLayoutMetrics,
  cx: &mut Context<EditorRibbon>,
) -> AnyElement {
  let invisibility_mode = command.selected;
  Button::new(("modern-ribbon-command", ribbon_command_key(command.id)))
    .xsmall()
    .compact()
    .outline()
    .h(metrics.chip_height)
    .w(metrics.chip_height)
    .icon(Icon::new(if invisibility_mode { IconName::EyeOff } else { IconName::Eye }).text_color(cx.theme().info))
    .selected(invisibility_mode)
    .tooltip("Invisibility mode")
    .on_click({
      let editor = editor.clone();
      move |_, _, cx| {
        editor.update(cx, |editor, cx| {
          editor.toggle_invisibility_mode(cx);
        });
      }
    })
    .into_any_element()
}
