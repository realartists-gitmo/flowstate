#[cfg(target_os = "macos")]
use gpui::ParentElement;
use gpui_component::button::Button;
use gpui_component::button::ButtonVariants as _;
use gpui_component::{Icon, IconName, Sizable as _};

#[derive(Clone, Copy)]
pub enum AppIcon {
  Close,
  NewFile,
  SaveFile,
  MultiPanel,
}

#[hotpath::measure]
pub fn icon_button(id: impl Into<gpui::ElementId>, icon: AppIcon) -> Button {
  if matches!(icon, AppIcon::SaveFile) {
    return Button::new(id)
      .icon(Icon::default().path("icons/save.svg"))
      .xsmall()
      .ghost();
  }
  platform_icon_button(Button::new(id), icon).xsmall().ghost()
}

#[cfg(target_os = "macos")]
#[hotpath::measure]
fn platform_icon_button(button: Button, icon: AppIcon) -> Button {
  let symbol = match icon {
    AppIcon::Close => "xmark",
    AppIcon::NewFile => "doc.badge.plus",
    AppIcon::SaveFile => "square.and.arrow.down",
    AppIcon::MultiPanel => "rectangle.split.2x1",
  };
  button.child(gpui_symbols::Icon::new(symbol).size(gpui::px(11.0)))
}

#[cfg(not(target_os = "macos"))]
#[hotpath::measure]
fn platform_icon_button(button: Button, icon: AppIcon) -> Button {
  let icon_name = match icon {
    AppIcon::Close => IconName::WindowClose,
    AppIcon::NewFile => IconName::Plus,
    AppIcon::SaveFile => IconName::File,
    AppIcon::MultiPanel => IconName::PanelRight,
  };
  button.icon(icon_name)
}
