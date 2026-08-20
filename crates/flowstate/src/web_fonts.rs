use std::borrow::Cow;

use gpui::App;

pub fn register(cx: &mut App) {
  cx.text_system()
    .add_fonts(vec![
      Cow::Borrowed(include_bytes!("../assets/fonts/Carlito-Regular.ttf").as_slice()),
      Cow::Borrowed(include_bytes!("../assets/fonts/Carlito-Bold.ttf").as_slice()),
      Cow::Borrowed(include_bytes!("../assets/fonts/Carlito-Italic.ttf").as_slice()),
      Cow::Borrowed(include_bytes!("../assets/fonts/Carlito-BoldItalic.ttf").as_slice()),
    ])
    .expect("bundled Carlito fonts must be valid");
}
