#[cfg(target_os = "windows")]
#[hotpath::measure]
fn windows_apply_capslock(text: &str) -> String {
  // GPUI 0.2.2's Windows key_char generation does not include Caps Lock in
  // the ToUnicode keyboard state. For normal letter input, Caps Lock inverts
  // the Shift-produced case; non-letter keys should pass through unchanged.
  let mut chars = text.chars();
  let Some(ch) = chars.next() else {
    return String::new();
  };
  if chars.next().is_none() && ch.is_ascii_alphabetic() {
    if ch.is_ascii_lowercase() {
      ch.to_ascii_uppercase().to_string()
    } else {
      ch.to_ascii_lowercase().to_string()
    }
  } else {
    text.to_string()
  }
}

