//! R1-B: recognized-style HTML paste. The clipboard's `text/html` slot is
//! inspected ONLY for the fixed style-name catalog the .docx importer cleans
//! into (Pocket/Hat/Block/Tag/Analytic/Undertag paragraphs; Cite/Emphasis/
//! Underline runs — Word writes custom style names as HTML classes). All
//! other formatting is discarded exactly as the plain-text path discards it,
//! and HTML with NO recognized names returns `None` so webpages keep pasting
//! plain. This is deliberately NOT an HTML interpreter: no link model, no
//! color mapping, no layout — just the style vocabulary Flowstate already
//! speaks.

use flowstate_document::{InputParagraph, InputRun, ParagraphStyle, RunStyles};

use crate::interpreter::{paragraph_style_from_canonical_name, run_semantic_from_canonical_name};

/// Paragraphs recovered from recognized-style HTML, or `None` when the HTML
/// carries no recognized names (the caller falls through to plain text).
#[must_use]
pub fn paragraphs_from_recognized_html(html: &str) -> Option<Vec<InputParagraph>> {
  let mut paragraphs = Vec::new();
  let mut recognized_anything = false;
  let mut cursor = 0;
  while let Some(open_rel) = html[cursor..].find('<') {
    let open = cursor + open_rel;
    let Some(close_rel) = html[open..].find('>') else {
      break;
    };
    let close = open + close_rel;
    let tag = &html[open + 1..close];
    cursor = close + 1;
    let name = tag_name(tag);
    if !matches!(name.as_str(), "p" | "h1" | "h2" | "h3" | "h4") {
      continue;
    }
    // Word writes the custom style as the class (spaces collapsed); the
    // canonicalizer is the SAME one the .docx importer uses.
    let style = class_attr(tag)
      .and_then(|class| paragraph_style_from_canonical_name(&class))
      .or_else(|| match name.as_str() {
        "h1" => paragraph_style_from_canonical_name("heading1"),
        "h2" => paragraph_style_from_canonical_name("heading2"),
        "h3" => paragraph_style_from_canonical_name("heading3"),
        "h4" => paragraph_style_from_canonical_name("heading4"),
        _ => None,
      });
    let end = find_close_tag(html, cursor, &name).unwrap_or(html.len());
    let body = &html[cursor..end];
    cursor = end;
    let (runs, runs_recognized) = runs_from_paragraph_body(body);
    if style.is_some() || runs_recognized {
      recognized_anything = true;
    }
    if runs.iter().all(|run| run.text.trim().is_empty()) && style.is_none() {
      continue;
    }
    paragraphs.push(InputParagraph {
      style: style.unwrap_or(ParagraphStyle::Normal),
      runs,
    });
  }
  (recognized_anything && !paragraphs.is_empty()).then_some(paragraphs)
}

/// Runs within one paragraph body: `<span class=…>` scopes carrying a
/// recognized run style keep it; every other tag is stripped. Returns the
/// runs plus whether any recognized run style appeared.
fn runs_from_paragraph_body(body: &str) -> (Vec<InputRun>, bool) {
  let mut runs: Vec<InputRun> = Vec::new();
  let mut stack: Vec<Option<RunStyles>> = Vec::new();
  let mut recognized = false;
  let mut text = String::new();
  let mut cursor = 0;

  let flush = |text: &mut String, styles: RunStyles, runs: &mut Vec<InputRun>| {
    if text.is_empty() {
      return;
    }
    let chunk = std::mem::take(text);
    if let Some(last) = runs.last_mut()
      && last.styles == styles
    {
      last.text.push_str(&chunk);
    } else {
      runs.push(InputRun { text: chunk, styles });
    }
  };

  while cursor < body.len() {
    let rest = &body[cursor..];
    if let Some(open_rel) = rest.find('<') {
      let literal = &rest[..open_rel];
      text.push_str(&decode_entities(literal));
      let open = cursor + open_rel;
      let Some(close_rel) = body[open..].find('>') else {
        break;
      };
      let tag = &body[open + 1..close_rel + open];
      cursor = open + close_rel + 1;
      let effective = |stack: &[Option<RunStyles>]| stack.iter().rev().find_map(|entry| *entry).unwrap_or_default();
      let name = tag_name(tag);
      match name.as_str() {
        "span" | "font" => {
          flush(&mut text, effective(&stack), &mut runs);
          let styles = class_attr(tag)
            .and_then(|class| run_semantic_from_canonical_name(&class))
            .map(|semantic| RunStyles {
              semantic,
              ..RunStyles::default()
            });
          if styles.is_some() {
            recognized = true;
          }
          stack.push(styles);
        },
        "/span" | "/font" => {
          flush(&mut text, effective(&stack), &mut runs);
          stack.pop();
        },
        "br" | "br/" => text.push(' '),
        _ => {},
      }
    } else {
      text.push_str(&decode_entities(rest));
      break;
    }
  }
  let effective = stack.iter().rev().find_map(|entry| *entry).unwrap_or_default();
  flush(&mut text, effective, &mut runs);
  (runs, recognized)
}

fn tag_name(tag: &str) -> String {
  let closing = tag.starts_with('/');
  let name = tag
    .trim_start_matches('/')
    .split(|ch: char| ch.is_whitespace() || ch == '>')
    .next()
    .unwrap_or("")
    .trim_end_matches('/')
    .to_ascii_lowercase();
  if closing { format!("/{name}") } else { name }
}

/// The first token of the tag's `class` attribute (`class=Pocket`,
/// `class="Pocket Extra"`, `class='Pocket'`).
fn class_attr(tag: &str) -> Option<String> {
  let lower = tag.to_ascii_lowercase();
  let at = lower.find("class=")?;
  let value = &tag[at + "class=".len()..];
  let value = value.trim_start();
  let token = if let Some(rest) = value.strip_prefix('"') {
    rest.split('"').next().unwrap_or("")
  } else if let Some(rest) = value.strip_prefix('\'') {
    rest.split('\'').next().unwrap_or("")
  } else {
    value
      .split(|ch: char| ch.is_whitespace() || ch == '>')
      .next()
      .unwrap_or("")
  };
  let token = token.split_whitespace().next().unwrap_or("");
  (!token.is_empty()).then(|| token.to_string())
}

/// Find the matching close tag (non-nesting scan — Word paragraphs do not
/// nest `<p>`), returning the byte offset of its `<`.
fn find_close_tag(html: &str, from: usize, name: &str) -> Option<usize> {
  let lower = html.to_ascii_lowercase();
  let needle = format!("</{name}");
  lower[from..].find(&needle).map(|rel| from + rel)
}

/// Minimal entity decode for clipboard HTML (the plain-text path never sees
/// entities; recognized paste must not leak `&amp;` into cards).
fn decode_entities(text: &str) -> String {
  let mut out = String::with_capacity(text.len());
  let mut rest = text;
  while let Some(at) = rest.find('&') {
    out.push_str(&rest[..at]);
    rest = &rest[at..];
    let semi = rest.find(';').filter(|end| *end <= 10);
    let Some(end) = semi else {
      out.push('&');
      rest = &rest[1..];
      continue;
    };
    let entity = &rest[1..end];
    match entity {
      "amp" => out.push('&'),
      "lt" => out.push('<'),
      "gt" => out.push('>'),
      "quot" => out.push('"'),
      "apos" | "#39" => out.push('\''),
      "nbsp" => out.push(' '),
      _ => {
        if let Some(code) = entity.strip_prefix("#x").or_else(|| entity.strip_prefix("#X")) {
          if let Some(ch) = u32::from_str_radix(code, 16).ok().and_then(char::from_u32) {
            out.push(ch);
          }
        } else if let Some(code) = entity.strip_prefix('#') {
          if let Some(ch) = code.parse::<u32>().ok().and_then(char::from_u32) {
            out.push(ch);
          }
        } else {
          out.push('&');
          out.push_str(entity);
          out.push(';');
        }
      },
    }
    rest = &rest[end + 1..];
  }
  out.push_str(rest);
  // Collapse the whitespace runs HTML source formatting introduces (newlines
  // between tags), preserving genuine single spaces.
  let mut collapsed = String::with_capacity(out.len());
  let mut last_space = false;
  for ch in out.chars() {
    if ch.is_whitespace() {
      if !last_space {
        collapsed.push(' ');
      }
      last_space = true;
    } else {
      collapsed.push(ch);
      last_space = false;
    }
  }
  collapsed
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn recognized_word_classes_map_to_slots() {
    let html = r#"<html><body>
      <p class=Pocket>AFF CASE</p>
      <p class="Tag">Warming is real</p>
      <p class=MsoNormal>Smith 24 &amp; Jones</p>
    </body></html>"#;
    let paragraphs = paragraphs_from_recognized_html(html).expect("recognized styles present");
    assert_eq!(paragraphs.len(), 3);
    assert_eq!(paragraphs[0].style, flowstate_document::PARAGRAPH_POCKET);
    assert_eq!(paragraphs[0].runs[0].text, "AFF CASE");
    assert_eq!(paragraphs[1].style, flowstate_document::PARAGRAPH_TAG);
    assert_eq!(paragraphs[2].style, ParagraphStyle::Normal);
    assert_eq!(paragraphs[2].runs[0].text, "Smith 24 & Jones");
  }

  #[test]
  fn recognized_run_spans_keep_semantics() {
    let html = r"<p class=Normal>Quote <span class=Cite>Smith 24</span> says <span class=Emphasis>a lot</span>.</p>";
    let paragraphs = paragraphs_from_recognized_html(html).expect("recognized run styles present");
    let runs = &paragraphs[0].runs;
    assert_eq!(runs[0].text, "Quote ");
    assert_eq!(runs[0].styles, RunStyles::default());
    assert_eq!(runs[1].text, "Smith 24");
    assert_eq!(runs[1].styles.semantic, flowstate_document::SEMANTIC_CITE);
    assert_eq!(runs[3].styles.semantic, flowstate_document::SEMANTIC_EMPHASIS);
  }

  #[test]
  fn webpages_stay_plain() {
    let html = r#"<html><body><p class="article-lede">Breaking <b>news</b> tonight.</p><div>more</div></body></html>"#;
    assert!(
      paragraphs_from_recognized_html(html).is_none(),
      "no recognized names ⇒ None ⇒ the plain-text path (flattening is a FEATURE)"
    );
  }

  #[test]
  fn heading_tags_map_like_the_importer() {
    let html = "<h1>POCKET</h1><p>body</p>";
    let paragraphs = paragraphs_from_recognized_html(html).expect("h1 is a recognized heading");
    assert_eq!(paragraphs[0].style, flowstate_document::PARAGRAPH_POCKET);
  }
}
