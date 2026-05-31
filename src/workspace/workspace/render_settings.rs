#[hotpath::measure_all]
impl Workspace {
  fn on_save(&mut self, _: &Save, window: &mut Window, cx: &mut Context<Self>) {
    self.save_active(window, cx);
  }

  fn render_settings_overlay(&self, overlay: WorkspaceSettingsOverlay, cx: &mut Context<Self>) -> impl IntoElement {
    let workspace = cx.entity().downgrade();
    let title = match overlay {
      WorkspaceSettingsOverlay::Styles => "Document",
      WorkspaceSettingsOverlay::Settings => "Settings",
    };
    let pages = match overlay {
      WorkspaceSettingsOverlay::Styles => self.document_style_pages(workspace),
      WorkspaceSettingsOverlay::Settings => self.workspace_settings_pages(workspace),
    };
    let settings_id = match overlay {
      WorkspaceSettingsOverlay::Styles => match self.document_style_section {
        DocumentStyleSection::Text => "document-popup-settings-text",
        DocumentStyleSection::Style => "document-popup-settings-style",
        DocumentStyleSection::Colors => "document-popup-settings-colors",
        DocumentStyleSection::Size => "document-popup-settings-size",
        DocumentStyleSection::Background => "document-popup-settings-background",
      },
      WorkspaceSettingsOverlay::Settings => match self.settings_section {
        WorkspaceSettingsSection::General => "app-popup-settings-general",
      },
    };

    div()
      .absolute()
      .top_0()
      .right_0()
      .bottom_0()
      .left_0()
      .bg(cx.theme().background.opacity(0.72))
      .flex()
      .items_center()
      .justify_center()
      .occlude()
      .on_mouse_down(MouseButton::Left, cx.listener(|workspace, _, _, cx| {
        workspace.settings_overlay = None;
        cx.stop_propagation();
        cx.notify();
      }))
      .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
      .child(
        v_flex()
          .w(px(840.0))
          .h(px(580.0))
          .max_w_full()
          .max_h_full()
          .overflow_hidden()
          .rounded_lg()
          .border_1()
          .border_color(cx.theme().border)
          .bg(cx.theme().popover)
          .shadow_lg()
          .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
          .child(
            h_flex()
              .h(px(42.0))
              .flex_none()
              .items_center()
              .justify_between()
              .px_4()
              .border_b_1()
              .border_color(cx.theme().border)
              .child(
                div()
                  .font_weight(gpui::FontWeight::SEMIBOLD)
                  .child(title),
              )
              .child(
                Button::new("close-settings-overlay")
                  .icon(IconName::Close)
                  .xsmall()
                  .ghost()
                  .tooltip("Close")
                  .on_click(cx.listener(|workspace, _, _, cx| {
                    workspace.settings_overlay = None;
                    cx.notify();
                  })),
              ),
          )
          .child(
            div().flex_1().overflow_hidden().child(
              Settings::new(settings_id)
                .sidebar_width(px(176.0))
                .selected_page(if overlay == WorkspaceSettingsOverlay::Styles {
                  self.document_style_section.index()
                } else {
                  self.settings_section.index()
                })
                .pages(pages),
            ),
          ),
      )
  }

  fn document_style_pages(&self, workspace: WeakEntity<Workspace>) -> Vec<SettingPage> {
    vec![
      SettingPage::new("Text")
        .default_open(true)
        .group(reset_document_style_section_group(workspace.clone(), DocumentStyleSection::Text))
        .group(
          SettingGroup::new()
            .title("Text")
            .item(font_family_item(workspace.clone()))
            .item(style_number_item(
              workspace.clone(),
              "Normal size (pt)",
              1.0,
              200.0,
              0.25,
              |theme| pixels_to_pt(theme.body_font_size),
              |theme, value| {
                theme.body_font_size = pt_to_pixels(value);
              },
            ))
            .item(style_bold_italic_item(
              workspace.clone(),
              "Normal",
              |theme| (theme.normal_bold, theme.normal_italic),
              |theme, bold, italic| {
                theme.normal_bold = bold;
                theme.normal_italic = italic;
              },
            )),
        ),
      SettingPage::new("Style")
        .group(reset_document_style_section_group(workspace.clone(), DocumentStyleSection::Style))
        .group(
          SettingGroup::new()
            .title("Style")
            .item(style_face_item(
              workspace.clone(),
              "Pocket",
              |theme| (theme.pocket_bold, theme.pocket_italic, theme.pocket_underline),
              |theme, bold, italic, underline| {
                theme.pocket_bold = bold;
                theme.pocket_italic = italic;
                theme.pocket_underline = underline;
              },
            ))
            .item(style_face_item(
              workspace.clone(),
              "Hat",
              |theme| (theme.hat_bold, theme.hat_italic, theme.hat_underline),
              |theme, bold, italic, underline| {
                theme.hat_bold = bold;
                theme.hat_italic = italic;
                theme.hat_underline = underline;
              },
            ))
            .item(style_face_item(
              workspace.clone(),
              "Block",
              |theme| (theme.block_bold, theme.block_italic, theme.block_underline),
              |theme, bold, italic, underline| {
                theme.block_bold = bold;
                theme.block_italic = italic;
                theme.block_underline = underline;
              },
            ))
            .item(style_face_item(
              workspace.clone(),
              "Tag",
              |theme| (theme.tag_bold, theme.tag_italic, theme.tag_underline),
              |theme, bold, italic, underline| {
                theme.tag_bold = bold;
                theme.tag_italic = italic;
                theme.tag_underline = underline;
              },
            ))
            .item(style_face_item(
              workspace.clone(),
              "Cite",
              |theme| (theme.cite_bold, theme.cite_italic, theme.cite_underline),
              |theme, bold, italic, underline| {
                theme.cite_bold = bold;
                theme.cite_italic = italic;
                theme.cite_underline = underline;
              },
            ))
            .item(style_face_item(
              workspace.clone(),
              "Condensed",
              |theme| (theme.condensed_bold, theme.condensed_italic, theme.condensed_underline),
              |theme, bold, italic, underline| {
                theme.condensed_bold = bold;
                theme.condensed_italic = italic;
                theme.condensed_underline = underline;
              },
            ))
            .item(style_face_item(
              workspace.clone(),
              "Ultra Condensed",
              |theme| {
                (
                  theme.ultracondensed_bold,
                  theme.ultracondensed_italic,
                  theme.ultracondensed_underline,
                )
              },
              |theme, bold, italic, underline| {
                theme.ultracondensed_bold = bold;
                theme.ultracondensed_italic = italic;
                theme.ultracondensed_underline = underline;
              },
            ))
            .item(style_face_item(
              workspace.clone(),
              "Emphasis",
              |theme| (theme.emphasis_bold, theme.emphasis_italic, theme.emphasis_underline),
              |theme, bold, italic, underline| {
                theme.emphasis_bold = bold;
                theme.emphasis_italic = italic;
                theme.emphasis_underline = underline;
              },
            ))
            .item(style_face_item(
              workspace.clone(),
              "Underline",
              |theme| (theme.underline_bold, theme.underline_italic, theme.underline_underline),
              |theme, bold, italic, underline| {
                theme.underline_bold = bold;
                theme.underline_italic = italic;
                theme.underline_underline = underline;
              },
            ))
            .item(style_face_item(
              workspace.clone(),
              "Analytic",
              |theme| (theme.analytic_bold, theme.analytic_italic, theme.analytic_underline),
              |theme, bold, italic, underline| {
                theme.analytic_bold = bold;
                theme.analytic_italic = italic;
                theme.analytic_underline = underline;
              },
            ))
            .item(style_face_item(
              workspace.clone(),
              "Undertag",
              |theme| (theme.undertag_bold, theme.undertag_italic, theme.undertag_underline),
              |theme, bold, italic, underline| {
                theme.undertag_bold = bold;
                theme.undertag_italic = italic;
                theme.undertag_underline = underline;
              },
            )),
        ),
      SettingPage::new("Colors")
        .group(reset_document_style_section_group(workspace.clone(), DocumentStyleSection::Colors))
        .group(
          SettingGroup::new()
            .title("Style Colors")
            .item(style_color_item(
              workspace.clone(),
              "Text",
              |theme| theme.default_text_color,
              |theme, value| theme.default_text_color = value,
            ))
            .item(style_color_item(
              workspace.clone(),
              "Pocket",
              |theme| theme.pocket_color,
              |theme, value| theme.pocket_color = value,
            ))
            .item(style_color_item(
              workspace.clone(),
              "Hat",
              |theme| theme.hat_color,
              |theme, value| theme.hat_color = value,
            ))
            .item(style_color_item(
              workspace.clone(),
              "Block",
              |theme| theme.block_color,
              |theme, value| theme.block_color = value,
            ))
            .item(style_color_item(
              workspace.clone(),
              "Tag",
              |theme| theme.tag_color,
              |theme, value| theme.tag_color = value,
            ))
            .item(style_color_item(
              workspace.clone(),
              "Cite",
              |theme| theme.cite_color,
              |theme, value| theme.cite_color = value,
            ))
            .item(style_color_item(
              workspace.clone(),
              "Condensed",
              |theme| theme.condensed_color,
              |theme, value| theme.condensed_color = value,
            ))
            .item(style_color_item(
              workspace.clone(),
              "Ultra Condensed",
              |theme| theme.ultracondensed_color,
              |theme, value| theme.ultracondensed_color = value,
            ))
            .item(style_color_item(
              workspace.clone(),
              "Emphasis",
              |theme| theme.emphasis_color,
              |theme, value| theme.emphasis_color = value,
            ))
            .item(style_color_item(
              workspace.clone(),
              "Underline",
              |theme| theme.underline_color,
              |theme, value| theme.underline_color = value,
            ))
            .item(style_color_item(
              workspace.clone(),
              "Analytic",
              |theme| theme.analytic_color,
              |theme, value| theme.analytic_color = value,
            ))
            .item(style_color_item(
              workspace.clone(),
              "Undertag",
              |theme| theme.undertag_color,
              |theme, value| theme.undertag_color = value,
            )),
        )
        .group(
          SettingGroup::new()
            .title("Highlights")
            .item(style_color_item(
              workspace.clone(),
              "Spoken highlight",
              |theme| theme.highlight_spoken,
              |theme, value| {
                theme.highlight_spoken = value;
              },
            ))
            .item(style_color_item(
              workspace.clone(),
              "Insert highlight",
              |theme| theme.highlight_insert,
              |theme, value| {
                theme.highlight_insert = value;
              },
            ))
            .item(style_color_item(
              workspace.clone(),
              "Alt highlight",
              |theme| theme.highlight_alternative,
              |theme, value| {
                theme.highlight_alternative = value;
              },
            )),
        ),
      SettingPage::new("Size")
        .group(reset_document_style_section_group(workspace.clone(), DocumentStyleSection::Size))
        .group(
          SettingGroup::new()
            .title("Size")
            .item(style_number_item(
              workspace.clone(),
              "Normal (pt)",
              1.0,
              200.0,
              0.25,
              |theme| pixels_to_pt(theme.body_font_size),
              |theme, value| theme.body_font_size = pt_to_pixels(value),
            ))
            .item(style_number_item(
              workspace.clone(),
              "Pocket (pt)",
              1.0,
              200.0,
              0.25,
              |theme| pixels_to_pt(theme.pocket_font_size),
              |theme, value| theme.pocket_font_size = pt_to_pixels(value),
            ))
            .item(style_number_item(
              workspace.clone(),
              "Hat (pt)",
              1.0,
              200.0,
              0.25,
              |theme| pixels_to_pt(theme.hat_font_size),
              |theme, value| theme.hat_font_size = pt_to_pixels(value),
            ))
            .item(style_number_item(
              workspace.clone(),
              "Block (pt)",
              1.0,
              200.0,
              0.25,
              |theme| pixels_to_pt(theme.block_font_size),
              |theme, value| theme.block_font_size = pt_to_pixels(value),
            ))
            .item(style_number_item(
              workspace.clone(),
              "Tag (pt)",
              1.0,
              200.0,
              0.25,
              |theme| pixels_to_pt(theme.tag_font_size),
              |theme, value| theme.tag_font_size = pt_to_pixels(value),
            ))
            .item(style_number_item(
              workspace.clone(),
              "Cite (pt)",
              1.0,
              200.0,
              0.25,
              |theme| pixels_to_pt(theme.cite_font_size),
              |theme, value| theme.cite_font_size = pt_to_pixels(value),
            ))
            .item(style_number_item(
              workspace.clone(),
              "Condensed (pt)",
              1.0,
              200.0,
              0.25,
              |theme| pixels_to_pt(theme.condensed_font_size),
              |theme, value| theme.condensed_font_size = pt_to_pixels(value),
            ))
            .item(style_number_item(
              workspace.clone(),
              "Ultra Condensed (pt)",
              1.0,
              200.0,
              0.25,
              |theme| pixels_to_pt(theme.ultracondensed_font_size),
              |theme, value| theme.ultracondensed_font_size = pt_to_pixels(value),
            ))
            .item(style_number_item(
              workspace.clone(),
              "Emphasis (pt)",
              1.0,
              200.0,
              0.25,
              |theme| pixels_to_pt(theme.cite_font_size),
              |theme, value| theme.cite_font_size = pt_to_pixels(value),
            ))
            .item(style_number_item(
              workspace.clone(),
              "Underline (pt)",
              1.0,
              200.0,
              0.25,
              |theme| pixels_to_pt(theme.body_font_size),
              |theme, value| theme.body_font_size = pt_to_pixels(value),
            ))
            .item(style_number_item(
              workspace.clone(),
              "Analytic (pt)",
              1.0,
              200.0,
              0.25,
              |theme| pixels_to_pt(theme.tag_font_size),
              |theme, value| theme.tag_font_size = pt_to_pixels(value),
            ))
            .item(style_number_item(
              workspace.clone(),
              "Undertag (pt)",
              1.0,
              200.0,
              0.25,
              |theme| pixels_to_pt(theme.undertag_font_size),
              |theme, value| theme.undertag_font_size = pt_to_pixels(value),
            )),
        ),
      SettingPage::new("Background")
        .group(reset_document_style_section_group(workspace.clone(), DocumentStyleSection::Background))
        .group(
          SettingGroup::new()
            .title("Background")
            .item(style_color_item(
              workspace.clone(),
              "Document background",
              |theme| theme.document_background_color,
              |theme, value| theme.document_background_color = value,
            )),
        ),
    ]
  }

  fn workspace_settings_pages(&self, workspace: WeakEntity<Workspace>) -> Vec<SettingPage> {
    vec![
      SettingPage::new("General")
        .default_open(true)
        .group(reset_workspace_settings_section_group(workspace.clone(), WorkspaceSettingsSection::General))
        .group(
          SettingGroup::new()
            .title("Editing")
            .item(smart_word_selection_item(workspace.clone()))
            .item(autosave_item(workspace.clone()))
            .item(send_to_document_directory_item(workspace.clone()))
            .item(send_custom_directory_item(workspace)),
        ),
    ]
  }

}

#[hotpath::measure]
fn reset_document_style_section_group(workspace: WeakEntity<Workspace>, section: DocumentStyleSection) -> SettingGroup {
  reset_section_delegate_group(move |cx| {
    let _ = workspace.update(cx, |workspace, cx| {
      workspace.document_style_section = section;
      workspace.reset_document_style_section(cx);
    });
  })
}

#[hotpath::measure]
fn reset_workspace_settings_section_group(workspace: WeakEntity<Workspace>, section: WorkspaceSettingsSection) -> SettingGroup {
  reset_section_delegate_group(move |cx| {
    let _ = workspace.update(cx, |workspace, cx| {
      workspace.settings_section = section;
      workspace.reset_workspace_settings_section(cx);
    });
  })
}

#[hotpath::measure]
fn reset_section_delegate_group(reset: impl Fn(&mut App) + 'static) -> SettingGroup {
  SettingGroup::new().h_0().overflow_hidden().item(
    SettingItem::new(
      "",
      SettingField::input(
        |_| SharedString::from("changed"),
        move |_, cx| {
          reset(cx);
        },
      )
      .default_value(SharedString::from("default"))
      .hidden(),
    )
  )
}

#[hotpath::measure_all]
impl Workspace {
  fn reset_document_style_section(&mut self, cx: &mut Context<Self>) {
    let section = self.document_style_section;
    let defaults = DocumentTheme::default();
    let mut theme = self
      .active_editor
      .as_ref()
      .map(|editor| editor.read(cx).document().theme.clone())
      .unwrap_or_else(load_document_theme);

    match section {
      DocumentStyleSection::Text => {
        theme.default_font_family = defaults.default_font_family;
        theme.body_font_size = defaults.body_font_size;
        theme.normal_bold = defaults.normal_bold;
        theme.normal_italic = defaults.normal_italic;
        theme.normal_underline = defaults.normal_underline;
      },
      DocumentStyleSection::Style => {
        theme.pocket_bold = defaults.pocket_bold;
        theme.pocket_italic = defaults.pocket_italic;
        theme.pocket_underline = defaults.pocket_underline;
        theme.hat_bold = defaults.hat_bold;
        theme.hat_italic = defaults.hat_italic;
        theme.hat_underline = defaults.hat_underline;
        theme.block_bold = defaults.block_bold;
        theme.block_italic = defaults.block_italic;
        theme.block_underline = defaults.block_underline;
        theme.tag_bold = defaults.tag_bold;
        theme.tag_italic = defaults.tag_italic;
        theme.tag_underline = defaults.tag_underline;
        theme.cite_bold = defaults.cite_bold;
        theme.cite_italic = defaults.cite_italic;
        theme.cite_underline = defaults.cite_underline;
        theme.condensed_bold = defaults.condensed_bold;
        theme.condensed_italic = defaults.condensed_italic;
        theme.condensed_underline = defaults.condensed_underline;
        theme.ultracondensed_bold = defaults.ultracondensed_bold;
        theme.ultracondensed_italic = defaults.ultracondensed_italic;
        theme.ultracondensed_underline = defaults.ultracondensed_underline;
        theme.emphasis_bold = defaults.emphasis_bold;
        theme.emphasis_italic = defaults.emphasis_italic;
        theme.emphasis_underline = defaults.emphasis_underline;
        theme.underline_bold = defaults.underline_bold;
        theme.underline_italic = defaults.underline_italic;
        theme.underline_underline = defaults.underline_underline;
        theme.analytic_bold = defaults.analytic_bold;
        theme.analytic_italic = defaults.analytic_italic;
        theme.analytic_underline = defaults.analytic_underline;
        theme.undertag_bold = defaults.undertag_bold;
        theme.undertag_italic = defaults.undertag_italic;
        theme.undertag_underline = defaults.undertag_underline;
      },
      DocumentStyleSection::Colors => {
        theme.default_text_color = defaults.default_text_color;
        theme.pocket_color = defaults.pocket_color;
        theme.hat_color = defaults.hat_color;
        theme.block_color = defaults.block_color;
        theme.tag_color = defaults.tag_color;
        theme.cite_color = defaults.cite_color;
        theme.condensed_color = defaults.condensed_color;
        theme.ultracondensed_color = defaults.ultracondensed_color;
        theme.emphasis_color = defaults.emphasis_color;
        theme.underline_color = defaults.underline_color;
        theme.analytic_color = defaults.analytic_color;
        theme.undertag_color = defaults.undertag_color;
        theme.highlight_spoken = defaults.highlight_spoken;
        theme.highlight_insert = defaults.highlight_insert;
        theme.highlight_alternative = defaults.highlight_alternative;
      },
      DocumentStyleSection::Size => {
        theme.body_font_size = defaults.body_font_size;
        theme.pocket_font_size = defaults.pocket_font_size;
        theme.hat_font_size = defaults.hat_font_size;
        theme.block_font_size = defaults.block_font_size;
        theme.tag_font_size = defaults.tag_font_size;
        theme.cite_font_size = defaults.cite_font_size;
        theme.condensed_font_size = defaults.condensed_font_size;
        theme.ultracondensed_font_size = defaults.ultracondensed_font_size;
        theme.undertag_font_size = defaults.undertag_font_size;
      },
      DocumentStyleSection::Background => {
        theme.document_background_color = defaults.document_background_color;
      },
    }

    let theme_for_save = theme.clone();
    cx.background_executor()
      .spawn(async move {
        if let Err(error) = save_document_theme(&theme_for_save) {
          eprintln!("failed to save document style settings: {error}");
        }
      })
      .detach();
    self.apply_document_theme_to_open_editors(theme, cx);
    cx.notify();
  }

  fn reset_workspace_settings_section(&mut self, cx: &mut Context<Self>) {
    match self.settings_section {
      WorkspaceSettingsSection::General => {
        self.autosave_enabled = false;
        self.autosave_document_generations.clear();
        self.autosave_flow_in_flight.clear();
        for panel in &self.document_panels {
          let editor = panel.read(cx).editor();
          editor.update(cx, |editor, cx| {
            editor.set_smart_word_selection(true, cx);
          });
        }
        cx.background_executor()
          .spawn(async move {
            if let Err(error) = save_smart_word_selection(true) {
              eprintln!("failed to save smart word selection setting: {error}");
            }
            if let Err(error) = save_autosave(false) {
              eprintln!("failed to save autosave setting: {error}");
            }
            if let Err(error) = save_send_to_document_directory(true) {
              eprintln!("failed to save send directory mode setting: {error}");
            }
            if let Err(error) = save_send_custom_directory(None) {
              eprintln!("failed to save send directory setting: {error}");
            }
          })
          .detach();
        cx.notify();
      },
    }
  }
}
