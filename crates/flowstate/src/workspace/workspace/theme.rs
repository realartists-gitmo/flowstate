fn apply_app_theme_config(theme_name: &str, window: Option<&mut Window>, cx: &mut App) -> bool {
  let Some(theme) = ThemeRegistry::global(cx).themes().get(theme_name).cloned() else {
    return false;
  };

  let mode = theme.mode;
  Theme::global_mut(cx).apply_config(&theme);
  Theme::change(mode, window, cx);
  cx.refresh_windows();
  true
}

fn apply_app_theme(theme_name: &str, window: Option<&mut Window>, cx: &mut App) {
  if !apply_app_theme_config(theme_name, window, cx) {
    return;
  }

  let theme_name = theme_name.to_string();
  cx.background_executor()
    .spawn(async move {
      if let Err(error) = save_theme_name(&theme_name) {
        eprintln!("failed to save theme setting: {error}");
      }
    })
    .detach();
}

#[hotpath::measure]
fn truncate_tab_title(title: &str, max_chars: usize) -> String {
  let mut chars = title.chars();
  let mut short = String::new();
  for _ in 0..max_chars {
    let Some(ch) = chars.next() else {
      return title.to_string();
    };
    short.push(ch);
  }

  if chars.next().is_some() {
    short.push_str("...");
  }
  short
}

#[hotpath::measure]
fn untitled_index(title: &str) -> Option<usize> {
  let title = title.strip_suffix(".db8").unwrap_or(title);
  let number = title.strip_prefix("Untitled")?;
  if number.is_empty() {
    return None;
  }
  number.parse::<usize>().ok().filter(|index| *index > 0)
}

#[hotpath::measure]
fn untitled_flow_index(title: &str) -> Option<usize> {
  let title = title.strip_suffix(".fl0").unwrap_or(title);
  let number = title.strip_prefix("Untitled")?;
  if number.is_empty() {
    return None;
  }
  number.parse::<usize>().ok().filter(|index| *index > 0)
}
