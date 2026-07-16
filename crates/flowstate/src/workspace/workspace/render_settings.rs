use crate::app_settings::DocumentThemeSettings;

macro_rules! flowstate_face_accessors {
  ($get:ident, $set:ident, $bold:ident, $italic:ident, $underline:ident) => {
    fn $get(theme: &DocumentTheme) -> (bool, bool, ThemeUnderline) {
      let settings = DocumentThemeSettings::from(theme);
      (settings.$bold, settings.$italic, settings.$underline.into())
    }

    fn $set(theme: &mut DocumentTheme, bold: bool, italic: bool, underline: ThemeUnderline) {
      let mut settings = DocumentThemeSettings::from(&*theme);
      settings.$bold = bold;
      settings.$italic = italic;
      settings.$underline = underline.into();
      *theme = settings.into();
    }
  };
}

macro_rules! flowstate_color_accessors {
  ($get:ident, $set:ident, $field:ident) => {
    fn $get(theme: &DocumentTheme) -> Hsla {
      DocumentThemeSettings::from(theme).$field.into()
    }

    fn $set(theme: &mut DocumentTheme, value: Hsla) {
      let mut settings = DocumentThemeSettings::from(&*theme);
      settings.$field = value.into();
      *theme = settings.into();
    }
  };
}

macro_rules! flowstate_size_accessors {
  ($get:ident, $set:ident, $field:ident) => {
    fn $get(theme: &DocumentTheme) -> f64 {
      pixels_to_pt(px(DocumentThemeSettings::from(theme).$field))
    }

    fn $set(theme: &mut DocumentTheme, value: f64) {
      let mut settings = DocumentThemeSettings::from(&*theme);
      settings.$field = pt_to_pixels(value).as_f32();
      *theme = settings.into();
    }
  };
}

flowstate_face_accessors!(get_pocket_face, set_pocket_face, pocket_bold, pocket_italic, pocket_underline);
flowstate_face_accessors!(get_hat_face, set_hat_face, hat_bold, hat_italic, hat_underline);
flowstate_face_accessors!(get_block_face, set_block_face, block_bold, block_italic, block_underline);
flowstate_face_accessors!(get_tag_face, set_tag_face, tag_bold, tag_italic, tag_underline);
flowstate_face_accessors!(get_cite_face, set_cite_face, cite_bold, cite_italic, cite_underline);
flowstate_face_accessors!(
  get_condensed_face,
  set_condensed_face,
  condensed_bold,
  condensed_italic,
  condensed_underline
);
flowstate_face_accessors!(
  get_ultracondensed_face,
  set_ultracondensed_face,
  ultracondensed_bold,
  ultracondensed_italic,
  ultracondensed_underline
);
flowstate_face_accessors!(get_emphasis_face, set_emphasis_face, emphasis_bold, emphasis_italic, emphasis_underline);
flowstate_face_accessors!(
  get_underline_face,
  set_underline_face,
  underline_bold,
  underline_italic,
  underline_underline
);
flowstate_face_accessors!(get_analytic_face, set_analytic_face, analytic_bold, analytic_italic, analytic_underline);
flowstate_face_accessors!(get_undertag_face, set_undertag_face, undertag_bold, undertag_italic, undertag_underline);

flowstate_color_accessors!(get_pocket_color, set_pocket_color, pocket_color);
flowstate_color_accessors!(get_hat_color, set_hat_color, hat_color);
flowstate_color_accessors!(get_block_color, set_block_color, block_color);
flowstate_color_accessors!(get_tag_color, set_tag_color, tag_color);
flowstate_color_accessors!(get_cite_color, set_cite_color, cite_color);
flowstate_color_accessors!(get_condensed_color, set_condensed_color, condensed_color);
flowstate_color_accessors!(get_ultracondensed_color, set_ultracondensed_color, ultracondensed_color);
flowstate_color_accessors!(get_emphasis_color, set_emphasis_color, emphasis_color);
flowstate_color_accessors!(get_underline_color, set_underline_color, underline_color);
flowstate_color_accessors!(get_analytic_color, set_analytic_color, analytic_color);
flowstate_color_accessors!(get_undertag_color, set_undertag_color, undertag_color);
flowstate_color_accessors!(get_highlight_spoken, set_highlight_spoken, highlight_spoken);
flowstate_color_accessors!(get_highlight_insert, set_highlight_insert, highlight_insert);
flowstate_color_accessors!(get_highlight_alternative, set_highlight_alternative, highlight_alternative);
flowstate_color_accessors!(get_highlight_marked, set_highlight_marked, highlight_marked);

flowstate_size_accessors!(get_pocket_size, set_pocket_size, pocket_font_size);
flowstate_size_accessors!(get_hat_size, set_hat_size, hat_font_size);
flowstate_size_accessors!(get_block_size, set_block_size, block_font_size);
flowstate_size_accessors!(get_tag_size, set_tag_size, tag_font_size);
flowstate_size_accessors!(get_cite_size, set_cite_size, cite_font_size);
flowstate_size_accessors!(get_condensed_size, set_condensed_size, condensed_font_size);
flowstate_size_accessors!(get_ultracondensed_size, set_ultracondensed_size, ultracondensed_font_size);
flowstate_size_accessors!(get_undertag_size, set_undertag_size, undertag_font_size);

fn paragraph_boxing(theme: &DocumentTheme, slot: u8) -> (bool, f64) {
  let style = flowstate_document::custom_paragraph_style(theme, slot);
  let width = style.border.map_or(px(1.0), |border| border.width);
  (style.border.is_some(), pixels_to_pt(width))
}

fn set_paragraph_boxing(theme: &mut DocumentTheme, slot: u8, enabled: bool, width_pt: f64) {
  let mut style = flowstate_document::custom_paragraph_style(theme, slot);
  if enabled {
    let existing = style.border.unwrap_or(CustomParagraphBorder {
      width: px(1.0),
      space_x: px(6.0),
      space_y: px(2.0),
    });
    style.border = Some(CustomParagraphBorder {
      width: pt_to_pixels(width_pt.max(0.0)),
      ..existing
    });
  } else {
    style.border = None;
  }
  theme.set_custom_paragraph_style(slot, style);
}

fn semantic_boxing(theme: &DocumentTheme, slot: u8) -> (bool, f64) {
  let style = flowstate_document::custom_semantic_style(theme, slot);
  let width = style.border_width.unwrap_or(px(1.0));
  (style.border_width.is_some(), pixels_to_pt(width))
}

fn set_semantic_boxing(theme: &mut DocumentTheme, slot: u8, enabled: bool, width_pt: f64) {
  let mut style = flowstate_document::custom_semantic_style(theme, slot);
  style.border_width = enabled.then(|| pt_to_pixels(width_pt.max(0.0)));
  theme.set_custom_semantic_style(slot, style);
}

macro_rules! paragraph_box_accessors {
  ($get:ident, $set:ident, $slot:literal) => {
    fn $get(theme: &DocumentTheme) -> (bool, f64) {
      paragraph_boxing(theme, $slot)
    }

    fn $set(theme: &mut DocumentTheme, enabled: bool, width_pt: f64) {
      set_paragraph_boxing(theme, $slot, enabled, width_pt);
    }
  };
}

macro_rules! semantic_box_accessors {
  ($get:ident, $set:ident, $slot:literal) => {
    fn $get(theme: &DocumentTheme) -> (bool, f64) {
      semantic_boxing(theme, $slot)
    }

    fn $set(theme: &mut DocumentTheme, enabled: bool, width_pt: f64) {
      set_semantic_boxing(theme, $slot, enabled, width_pt);
    }
  };
}

paragraph_box_accessors!(get_pocket_box, set_pocket_box, 0);
paragraph_box_accessors!(get_hat_box, set_hat_box, 1);
paragraph_box_accessors!(get_block_box, set_block_box, 2);
paragraph_box_accessors!(get_tag_box, set_tag_box, 3);
paragraph_box_accessors!(get_analytic_box, set_analytic_box, 4);
paragraph_box_accessors!(get_undertag_box, set_undertag_box, 6);
semantic_box_accessors!(get_cite_box, set_cite_box, 1);
semantic_box_accessors!(get_emphasis_box, set_emphasis_box, 2);
semantic_box_accessors!(get_underline_box, set_underline_box, 3);
semantic_box_accessors!(get_condensed_box, set_condensed_box, 4);
semantic_box_accessors!(get_ultracondensed_box, set_ultracondensed_box, 5);

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
        WorkspaceSettingsSection::Appearance => "app-popup-settings-appearance",
        WorkspaceSettingsSection::Collaboration => "app-popup-settings-collaboration",
        WorkspaceSettingsSection::Keymap => "app-popup-settings-keymap",
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
      // P5-S6: while a keymap row is recording, the next chord pressed
      // anywhere in the overlay becomes the binding (Escape cancels).
      .on_key_down(cx.listener(|workspace, event: &gpui::KeyDownEvent, _, cx| {
        if workspace.handle_keymap_recording_key(event, cx) {
          cx.stop_propagation();
          return;
        }
        // P5-S2 (keyboard law): Escape closes the settings overlay.
        if event.keystroke.key.as_str() == "escape" {
          workspace.settings_overlay = None;
          cx.notify();
          cx.stop_propagation();
        }
      }))
      .on_mouse_down(
        MouseButton::Left,
        cx.listener(|workspace, _, _, cx| {
          workspace.settings_overlay = None;
          cx.stop_propagation();
          cx.notify();
        }),
      )
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
              .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(title))
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
        .description("Stored inside this document — collaborators see these styles.")
        .default_open(true)
        .group(reset_document_style_section_group(workspace.clone(), DocumentStyleSection::Text))
        // P5-S4: the live specimen leads — every control below repaints it.
        .group(SettingGroup::new().title("Specimen").item(style_specimen_item(workspace.clone())))
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
              get_pocket_face,
              set_pocket_face,
              get_pocket_box,
              set_pocket_box,
            ))
            .item(style_face_item(
              workspace.clone(),
              "Hat",
              get_hat_face,
              set_hat_face,
              get_hat_box,
              set_hat_box,
            ))
            .item(style_face_item(
              workspace.clone(),
              "Block",
              get_block_face,
              set_block_face,
              get_block_box,
              set_block_box,
            ))
            .item(style_face_item(
              workspace.clone(),
              "Tag",
              get_tag_face,
              set_tag_face,
              get_tag_box,
              set_tag_box,
            ))
            .item(style_face_item(
              workspace.clone(),
              "Cite",
              get_cite_face,
              set_cite_face,
              get_cite_box,
              set_cite_box,
            ))
            .item(style_face_item(
              workspace.clone(),
              "Shrink",
              get_condensed_face,
              set_condensed_face,
              get_condensed_box,
              set_condensed_box,
            ))
            .item(style_face_item(
              workspace.clone(),
              "Ultra Shrink",
              get_ultracondensed_face,
              set_ultracondensed_face,
              get_ultracondensed_box,
              set_ultracondensed_box,
            ))
            .item(style_face_item(
              workspace.clone(),
              "Emphasis",
              get_emphasis_face,
              set_emphasis_face,
              get_emphasis_box,
              set_emphasis_box,
            ))
            .item(style_face_item(
              workspace.clone(),
              "Underline",
              get_underline_face,
              set_underline_face,
              get_underline_box,
              set_underline_box,
            ))
            .item(style_face_item(
              workspace.clone(),
              "Analytic",
              get_analytic_face,
              set_analytic_face,
              get_analytic_box,
              set_analytic_box,
            ))
            .item(style_face_item(
              workspace.clone(),
              "Undertag",
              get_undertag_face,
              set_undertag_face,
              get_undertag_box,
              set_undertag_box,
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
            .item(style_color_item(workspace.clone(), "Pocket", get_pocket_color, set_pocket_color))
            .item(style_color_item(workspace.clone(), "Hat", get_hat_color, set_hat_color))
            .item(style_color_item(workspace.clone(), "Block", get_block_color, set_block_color))
            .item(style_color_item(workspace.clone(), "Tag", get_tag_color, set_tag_color))
            .item(style_color_item(workspace.clone(), "Cite", get_cite_color, set_cite_color))
            .item(style_color_item(workspace.clone(), "Shrink", get_condensed_color, set_condensed_color))
            .item(style_color_item(
              workspace.clone(),
              "Ultra Shrink",
              get_ultracondensed_color,
              set_ultracondensed_color,
            ))
            .item(style_color_item(workspace.clone(), "Emphasis", get_emphasis_color, set_emphasis_color))
            .item(style_color_item(workspace.clone(), "Underline", get_underline_color, set_underline_color))
            .item(style_color_item(workspace.clone(), "Analytic", get_analytic_color, set_analytic_color))
            .item(style_color_item(workspace.clone(), "Undertag", get_undertag_color, set_undertag_color)),
        )
        .group(
          SettingGroup::new()
            .title("Highlights")
            .item(style_color_item(
              workspace.clone(),
              "Spoken highlight",
              get_highlight_spoken,
              set_highlight_spoken,
            ))
            .item(style_color_item(
              workspace.clone(),
              "Insert highlight",
              get_highlight_insert,
              set_highlight_insert,
            ))
            .item(style_color_item(
              workspace.clone(),
              "Alt highlight",
              get_highlight_alternative,
              set_highlight_alternative,
            ))
            .item(style_color_item(
              workspace.clone(),
              "Marked highlight",
              get_highlight_marked,
              set_highlight_marked,
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
              get_pocket_size,
              set_pocket_size,
            ))
            .item(style_number_item(
              workspace.clone(),
              "Hat (pt)",
              1.0,
              200.0,
              0.25,
              get_hat_size,
              set_hat_size,
            ))
            .item(style_number_item(
              workspace.clone(),
              "Block (pt)",
              1.0,
              200.0,
              0.25,
              get_block_size,
              set_block_size,
            ))
            .item(style_number_item(
              workspace.clone(),
              "Tag (pt)",
              1.0,
              200.0,
              0.25,
              get_tag_size,
              set_tag_size,
            ))
            .item(style_number_item(
              workspace.clone(),
              "Cite (pt)",
              1.0,
              200.0,
              0.25,
              get_cite_size,
              set_cite_size,
            ))
            .item(style_number_item(
              workspace.clone(),
              "Shrink (pt)",
              1.0,
              200.0,
              0.25,
              get_condensed_size,
              set_condensed_size,
            ))
            .item(style_number_item(
              workspace.clone(),
              "Ultra Shrink (pt)",
              1.0,
              200.0,
              0.25,
              get_ultracondensed_size,
              set_ultracondensed_size,
            ))
            .item(style_number_item(
              workspace.clone(),
              "Emphasis (pt)",
              1.0,
              200.0,
              0.25,
              get_cite_size,
              set_cite_size,
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
              get_tag_size,
              set_tag_size,
            ))
            .item(style_number_item(
              workspace.clone(),
              "Undertag (pt)",
              1.0,
              200.0,
              0.25,
              get_undertag_size,
              set_undertag_size,
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
        .description("Applies to this machine — every document.")
        .default_open(true)
        .group(reset_workspace_settings_section_group(
          workspace.clone(),
          WorkspaceSettingsSection::General,
        ))
        .group(
          SettingGroup::new()
            .title("Editing")
            .item(smart_word_selection_item(workspace.clone()))
            .item(autosave_item(workspace.clone()))
            .item(send_to_document_directory_item(workspace.clone()))
            .item(send_custom_directory_item(workspace.clone())),
        )
        .group(
          SettingGroup::new()
            .title("Workspace")
            .item(tub_root_item(workspace.clone())),
        ),
      // P5-S2: theme moved in from the Flowstate menu — Appearance is its home.
      SettingPage::new("Appearance")
        .description("Applies to this machine — every document. Document styles live under Document ▸ Styles.")
        .group(
          SettingGroup::new()
            .title("Theme")
            .description("Applies immediately.")
            .item(theme_picker_item(workspace.clone())),
        ),
      SettingPage::new("Collaboration")
        .description("Applies to this machine — identity, trust, and transports.")
        .group(reset_workspace_settings_section_group(
          workspace.clone(),
          WorkspaceSettingsSection::Collaboration,
        ))
        // CO-S2: identity, trust, squads, and discovery moved to the Collab
        // Hub (Collaborate ▸ Share / the status pill) — where the people are.
        .group(
          SettingGroup::new()
            .title("People and discovery")
            .description("Moved to the Collab Hub — open it from the Collaborate menu or the status bar."),
        )
        .group(
          SettingGroup::new()
            .title("Dropbox")
            .item(dropbox_connection_item(workspace.clone()))
            .item(dropbox_document_binding_item(workspace.clone())),
        ),
      SettingPage::new("Keymap")
        .description("Applies to this machine — every document.")
        .group(reset_workspace_settings_section_group(
          workspace.clone(),
          WorkspaceSettingsSection::Keymap,
        ))
        .group(
          SettingGroup::new()
            .title("Exchange")
            .item(keymap_exchange_item(workspace.clone())),
        )
        .group(
          SettingGroup::new()
            .title("Keyboard shortcuts")
            .item(keymap_editor_item(workspace)),
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
  SettingGroup::new()
    .h_0()
    .overflow_hidden()
    .item(SettingItem::new(
      "",
      SettingField::input(
        |_| SharedString::from("changed"),
        move |_, cx| {
          reset(cx);
        },
      )
      .default_value(SharedString::from("default"))
      .hidden(),
    ))
}

#[hotpath::measure_all]
impl Workspace {
  fn reset_document_style_section(&mut self, cx: &mut Context<Self>) {
    let section = self.document_style_section;
    let defaults = flowstate_document_theme();
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
        theme.custom_paragraph_styles = defaults.custom_paragraph_styles;
        theme.custom_semantic_styles = defaults.custom_semantic_styles;
      },
      DocumentStyleSection::Colors => {
        theme.default_text_color = defaults.default_text_color;
        theme.custom_paragraph_styles = defaults.custom_paragraph_styles;
        theme.custom_semantic_styles = defaults.custom_semantic_styles;
        theme.custom_highlight_styles = defaults.custom_highlight_styles;
      },
      DocumentStyleSection::Size => {
        theme.body_font_size = defaults.body_font_size;
        theme.custom_paragraph_styles = defaults.custom_paragraph_styles;
        theme.custom_semantic_styles = defaults.custom_semantic_styles;
      },
      DocumentStyleSection::Background => {
        theme.document_background_color = defaults.document_background_color;
      },
    }

    if matches!(section, DocumentStyleSection::Colors | DocumentStyleSection::Background) {
      self.document_style_picker_revision = self.document_style_picker_revision.wrapping_add(1);
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
      // P5-S2: Appearance's reset returns to the registry's default light theme.
      WorkspaceSettingsSection::Appearance => {
        let default_theme = gpui_component::ThemeRegistry::global(cx).default_light_theme().name.to_string();
        let workspace = cx.entity().downgrade();
        apply_app_theme(&default_theme, workspace, None, cx);
      },
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
      WorkspaceSettingsSection::Keymap => {
        let entries = crate::commands::Keymap::defaults().entries;
        cx.background_executor()
          .spawn(async move {
            if let Err(error) = crate::app_settings::save_keymap_entries(entries) {
              eprintln!("failed to reset keymap: {error}");
            }
          })
          .detach();
        cx.notify();
      },
      WorkspaceSettingsSection::Collaboration => {
        if let Err(error) = crate::app_settings::save_collaboration_discovery_options(false, false) {
          tracing::warn!(%error, "resetting collaboration discovery settings failed");
        }
        crate::collab::reconfigure_discovery(cx);
        cx.notify();
      },
    }
  }
}


/// P5-S2: the theme picker, live-applied (moved in from the Flowstate menu).
#[hotpath::measure]
fn theme_picker_item(workspace: WeakEntity<Workspace>) -> SettingItem {
  SettingItem::render(move |_, _, cx| {
    let current_theme = gpui_component::Theme::global(cx).theme_name().to_string();
    let theme_names = gpui_component::ThemeRegistry::global(cx)
      .sorted_themes()
      .into_iter()
      .map(|theme| theme.name.to_string());
    let workspace = workspace.clone();
    div()
      .w_full()
      .flex()
      .flex_wrap()
      .gap_1()
      .children(theme_names.enumerate().map(|(ix, theme_name)| {
        let selected = theme_name == current_theme;
        let apply_theme = theme_name.clone();
        let workspace = workspace.clone();
        Button::new(("appearance-theme", ix))
          .xsmall()
          .when(selected, |this| this.primary())
          .when(!selected, |this| this.ghost())
          .label(theme_name)
          .on_click(move |_, window, cx| {
            apply_app_theme(&apply_theme, workspace.clone(), Some(window), cx);
          })
      }))
      .into_any_element()
  })
}

/// P5-S2: where the tub lives, with a change affordance (the old buried row).
#[hotpath::measure]
fn tub_root_item(workspace: WeakEntity<Workspace>) -> SettingItem {
  SettingItem::render(move |_, _, cx| {
    let root_label: SharedString = workspace
      .upgrade()
      .and_then(|workspace| workspace.read(cx).tub_root.clone())
      .map_or_else(
        || SharedString::from("No tub selected"),
        |root| root.to_string_lossy().into_owned().into(),
      );
    let change_workspace = workspace.clone();
    h_flex()
      .w_full()
      .gap_2()
      .items_center()
      .child(div().text_sm().text_color(cx.theme().muted_foreground).flex_1().min_w_0().overflow_hidden().text_ellipsis().child(root_label))
      .child(
        Button::new("settings-tub-root")
          .xsmall()
          .label("Change tub…")
          .on_click(move |_, window, cx| {
            let _ = change_workspace.update(cx, |workspace, cx| workspace.prompt_select_tub(window, cx));
          }),
      )
      .into_any_element()
  })
}
