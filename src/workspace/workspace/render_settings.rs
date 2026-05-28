#[hotpath::measure_all]
impl Workspace {
  fn on_save(&mut self, _: &Save, window: &mut Window, cx: &mut Context<Self>) {
    self.save_active(window, cx);
  }

  fn render_styles_settings_view(&self, cx: &mut Context<Self>) -> impl IntoElement {
    let workspace = cx.entity().downgrade();
    let has_document = self.active_editor.is_some();

    v_flex()
      .flex_1()
      .overflow_hidden()
      .bg(cx.theme().background)
      .child(
        h_flex()
          .h(px(44.0))
          .flex_none()
          .items_center()
          .justify_between()
          .px_4()
          .border_b_1()
          .border_color(cx.theme().border)
          .child(
            div()
              .font_weight(gpui::FontWeight::SEMIBOLD)
              .child("Document Style Settings"),
          )
          .child(
            Button::new("close-styles-settings")
              .icon(IconName::Close)
              .label("Close")
              .small()
              .ghost()
              .on_click(cx.listener(|workspace, _, _, cx| {
                workspace.styles_settings_open = false;
                cx.notify();
              })),
          ),
      )
      .child(
        div().flex_1().overflow_hidden().child(
          Settings::new("document-style-settings")
            .sidebar_width(px(220.0))
            .pages(self.document_style_pages(workspace, has_document)),
        ),
      )
  }

  fn document_style_pages(&self, workspace: WeakEntity<Workspace>, has_document: bool) -> Vec<SettingPage> {
    vec![
      SettingPage::new("Base")
        .default_open(true)
        .description(if has_document {
          "Base font and normal text."
        } else {
          "Open a document to preview style values."
        })
        .resettable(false)
        .group(
          SettingGroup::new()
            .title("Editing")
            .description("Selection behavior for text editing.")
            .item(smart_word_selection_item(workspace.clone())),
        )
        .group(
          SettingGroup::new()
            .title("Apply to All")
            .description("Blank fields are left unchanged when Apply is pressed.")
            .item(SettingItem::render({
              let workspace = workspace.clone();
              move |_, window, cx| render_apply_all_styles(workspace.clone(), window, cx)
            })),
        )
        .group(
          SettingGroup::new()
            .title("Text")
            .description(if has_document {
              "Base font and normal text."
            } else {
              "Open a document to preview style values."
            })
            .item(font_family_item(workspace.clone()))
            .item(style_color_item(
              workspace.clone(),
              "Text color",
              |theme| theme.default_text_color,
              |theme, value| {
                theme.default_text_color = value;
              },
            ))
            .item(style_number_item(
              workspace.clone(),
              "Body size (pt)",
              1.0,
              200.0,
              0.25,
              |theme| pixels_to_pt(theme.body_font_size),
              |theme, value| {
                theme.body_font_size = pt_to_pixels(value);
              },
            ))
            .item(style_face_item(
              workspace.clone(),
              "Normal",
              |theme| (theme.normal_bold, theme.normal_italic, theme.normal_underline),
              |theme, bold, italic, underline| {
                theme.normal_bold = bold;
                theme.normal_italic = italic;
                theme.normal_underline = underline;
              },
            )),
        ),
      SettingPage::new("Paragraph")
        .description("Visual treatment for paragraph-level semantic styles.")
        .resettable(false)
        .group(
          SettingGroup::new()
            .title("Paragraph Styles")
            .item(style_compact_item(
              workspace.clone(),
              "Pocket",
              |theme| pixels_to_pt(theme.pocket_font_size),
              |theme, value| theme.pocket_font_size = pt_to_pixels(value),
              Some((|theme| theme.pocket_color, |theme, value| theme.pocket_color = value)),
              |theme| (theme.pocket_bold, theme.pocket_italic, theme.pocket_underline),
              |theme, bold, italic, underline| {
                theme.pocket_bold = bold;
                theme.pocket_italic = italic;
                theme.pocket_underline = underline;
              },
            ))
            .item(style_compact_item(
              workspace.clone(),
              "Hat",
              |theme| pixels_to_pt(theme.hat_font_size),
              |theme, value| theme.hat_font_size = pt_to_pixels(value),
              Some((|theme| theme.hat_color, |theme, value| theme.hat_color = value)),
              |theme| (theme.hat_bold, theme.hat_italic, theme.hat_underline),
              |theme, bold, italic, underline| {
                theme.hat_bold = bold;
                theme.hat_italic = italic;
                theme.hat_underline = underline;
              },
            ))
            .item(style_compact_item(
              workspace.clone(),
              "Block",
              |theme| pixels_to_pt(theme.block_font_size),
              |theme, value| theme.block_font_size = pt_to_pixels(value),
              Some((|theme| theme.block_color, |theme, value| theme.block_color = value)),
              |theme| (theme.block_bold, theme.block_italic, theme.block_underline),
              |theme, bold, italic, underline| {
                theme.block_bold = bold;
                theme.block_italic = italic;
                theme.block_underline = underline;
              },
            ))
            .item(style_compact_item(
              workspace.clone(),
              "Tag",
              |theme| pixels_to_pt(theme.tag_font_size),
              |theme, value| theme.tag_font_size = pt_to_pixels(value),
              Some((|theme| theme.tag_color, |theme, value| theme.tag_color = value)),
              |theme| (theme.tag_bold, theme.tag_italic, theme.tag_underline),
              |theme, bold, italic, underline| {
                theme.tag_bold = bold;
                theme.tag_italic = italic;
                theme.tag_underline = underline;
              },
            ))
            .item(style_compact_item(
              workspace.clone(),
              "Analytic",
              |theme| pixels_to_pt(theme.tag_font_size),
              |theme, value| theme.tag_font_size = pt_to_pixels(value),
              Some((|theme| theme.analytic_color, |theme, value| theme.analytic_color = value)),
              |theme| (theme.analytic_bold, theme.analytic_italic, theme.analytic_underline),
              |theme, bold, italic, underline| {
                theme.analytic_bold = bold;
                theme.analytic_italic = italic;
                theme.analytic_underline = underline;
              },
            ))
            .item(style_compact_item(
              workspace.clone(),
              "Undertag",
              |theme| pixels_to_pt(theme.undertag_font_size),
              |theme, value| theme.undertag_font_size = pt_to_pixels(value),
              Some((|theme| theme.undertag_color, |theme, value| theme.undertag_color = value)),
              |theme| (theme.undertag_bold, theme.undertag_italic, theme.undertag_underline),
              |theme, bold, italic, underline| {
                theme.undertag_bold = bold;
                theme.undertag_italic = italic;
                theme.undertag_underline = underline;
              },
            )),
        ),
      SettingPage::new("Run")
        .description("Visual treatment for inline semantic styles.")
        .resettable(false)
        .group(
          SettingGroup::new()
            .title("Run Styles")
            .item(style_compact_item(
              workspace.clone(),
              "Cite",
              |theme| pixels_to_pt(theme.cite_font_size),
              |theme, value| theme.cite_font_size = pt_to_pixels(value),
              Some((|theme| theme.cite_color, |theme, value| theme.cite_color = value)),
              |theme| (theme.cite_bold, theme.cite_italic, theme.cite_underline),
              |theme, bold, italic, underline| {
                theme.cite_bold = bold;
                theme.cite_italic = italic;
                theme.cite_underline = underline;
              },
            ))
            .item(style_compact_item(
              workspace.clone(),
              "Underline",
              |theme| pixels_to_pt(theme.body_font_size),
              |theme, value| theme.body_font_size = pt_to_pixels(value),
              Some((|theme| theme.underline_color, |theme, value| theme.underline_color = value)),
              |theme| (theme.underline_bold, theme.underline_italic, theme.underline_underline),
              |theme, bold, italic, underline| {
                theme.underline_bold = bold;
                theme.underline_italic = italic;
                theme.underline_underline = underline;
              },
            ))
            .item(style_compact_item(
              workspace.clone(),
              "Emphasis",
              |theme| pixels_to_pt(theme.cite_font_size),
              |theme, value| theme.cite_font_size = pt_to_pixels(value),
              Some((|theme| theme.emphasis_color, |theme, value| theme.emphasis_color = value)),
              |theme| (theme.emphasis_bold, theme.emphasis_italic, theme.emphasis_underline),
              |theme, bold, italic, underline| {
                theme.emphasis_bold = bold;
                theme.emphasis_italic = italic;
                theme.emphasis_underline = underline;
              },
            ))
            .item(style_compact_item(
              workspace.clone(),
              "Condensed",
              |theme| pixels_to_pt(theme.condensed_font_size),
              |theme, value| theme.condensed_font_size = pt_to_pixels(value),
              Some((|theme| theme.condensed_color, |theme, value| theme.condensed_color = value)),
              |theme| (theme.condensed_bold, theme.condensed_italic, theme.condensed_underline),
              |theme, bold, italic, underline| {
                theme.condensed_bold = bold;
                theme.condensed_italic = italic;
                theme.condensed_underline = underline;
              },
            ))
            .item(style_compact_item(
              workspace.clone(),
              "Ultra-condensed",
              |theme| pixels_to_pt(theme.ultracondensed_font_size),
              |theme, value| theme.ultracondensed_font_size = pt_to_pixels(value),
              Some((|theme| theme.ultracondensed_color, |theme, value| theme.ultracondensed_color = value)),
              |theme| (theme.ultracondensed_bold, theme.ultracondensed_italic, theme.ultracondensed_underline),
              |theme, bold, italic, underline| {
                theme.ultracondensed_bold = bold;
                theme.ultracondensed_italic = italic;
                theme.ultracondensed_underline = underline;
              },
            )),
        ),
      SettingPage::new("Highlights")
        .description("Colors used by highlight semantic styles.")
        .resettable(false)
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
              "Alternative highlight",
              |theme| theme.highlight_alternative,
              |theme, value| {
                theme.highlight_alternative = value;
              },
            )),
        ),
    ]
  }

}
