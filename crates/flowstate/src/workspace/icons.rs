use gpui_component::button::Button;
use gpui_component::button::ButtonVariants as _;
use gpui_component::{Icon, IconName, Sizable as _};

#[derive(Clone, Copy)]
pub enum AppIcon {
  Close,
  NewFile,
  SaveFile,
  TabLeft,
  TabRight,
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

#[hotpath::measure]
fn platform_icon_button(button: Button, icon: AppIcon) -> Button {
  let icon_name = match icon {
    AppIcon::Close => IconName::WindowClose,
    AppIcon::NewFile => IconName::Plus,
    AppIcon::SaveFile => IconName::File,
    AppIcon::TabLeft => IconName::ChevronLeft,
    AppIcon::TabRight => IconName::ChevronRight,
    AppIcon::MultiPanel => IconName::PanelRight,
  };
  button.icon(icon_name)
}
