//! P4-S4/R2: the shared ribbon dress — the group/chip visual language both
//! ribbons speak. The .db8 ribbon's chips grew this idiom first
//! (`modern_command_chip`); the flow ribbon is the second consumer, so the
//! generic pieces live here: a divided group container and a chip builder
//! with icon + tooltip + shortcut hint + selected/danger states.

use gpui::{App, Div, ElementId, SharedString, div, prelude::*, px};
use gpui_component::{
  ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _,
  button::Button,
};

pub const CHIP_HEIGHT: gpui::Pixels = px(26.0);

/// A ribbon group: chips flow in a row; every group after the first carries
/// the divider (the same hairline the .db8 ribbon draws between groups).
pub fn ribbon_group(divider: bool, cx: &App) -> Div {
  div()
    .flex()
    .flex_row()
    .flex_none()
    .items_center()
    .gap_1()
    .when(divider, |this| {
      this
        .pl(px(10.0))
        .border_l_1()
        .border_color(cx.theme().border.opacity(0.72))
    })
}

/// One ribbon chip in the shared dress. `shortcut` renders into the tooltip
/// (the label band died in P4-S2 — tooltips carry the words).
pub struct RibbonChip {
  pub id: ElementId,
  pub label: SharedString,
  pub icon_path: Option<&'static str>,
  pub tooltip: SharedString,
  pub shortcut: Option<String>,
  pub selected: bool,
  pub disabled: bool,
  pub danger: bool,
}

impl RibbonChip {
  pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>, tooltip: impl Into<SharedString>) -> Self {
    Self {
      id: id.into(),
      label: label.into(),
      icon_path: None,
      tooltip: tooltip.into(),
      shortcut: None,
      selected: false,
      disabled: false,
      danger: false,
    }
  }

  #[must_use]
  pub fn icon(mut self, path: &'static str) -> Self {
    self.icon_path = Some(path);
    self
  }

  /// Tooltip shows the binding for `command` when one is active.
  #[must_use]
  pub fn command_shortcut(mut self, command: crate::commands::CommandId) -> Self {
    self.shortcut = crate::commands::active_keys_for(command).first().cloned();
    self
  }

  #[must_use]
  pub fn selected(mut self, selected: bool) -> Self {
    self.selected = selected;
    self
  }

  #[must_use]
  pub fn disabled(mut self, disabled: bool) -> Self {
    self.disabled = disabled;
    self
  }

  #[must_use]
  pub fn danger(mut self, danger: bool) -> Self {
    self.danger = danger;
    self
  }

  pub fn build(self, cx: &App) -> Button {
    let color = if self.danger { cx.theme().danger } else { cx.theme().foreground };
    let tooltip: SharedString = match &self.shortcut {
      Some(shortcut) => format!("{} ({shortcut})", self.tooltip).into(),
      None => self.tooltip.clone(),
    };
    Button::new(self.id)
      .xsmall()
      .compact()
      .outline()
      .h(CHIP_HEIGHT)
      .px(px(7.0))
      .rounded(cx.theme().radius)
      .selected(self.selected)
      .disabled(self.disabled)
      .text_color(color)
      .tooltip(tooltip)
      .when(self.selected, |this| {
        this.border_color(color).bg(color.opacity(0.18)).text_color(color)
      })
      .when_some(self.icon_path, |this, path| {
        this.icon(Icon::default().path(path).with_size(px(13.0)).text_color(color))
      })
      .label(self.label)
  }
}
