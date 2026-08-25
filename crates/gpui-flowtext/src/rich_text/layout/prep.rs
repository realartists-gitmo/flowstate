use std::sync::Arc;

use crop::Rope;

// §act-nine A9.3: prep validity is CONTENT-keyed — (style, version) via
// `ParagraphCacheKey` plus the invisibility mode. There is deliberately NO
// global `edit_generation` here: an edit to one paragraph must not invalidate
// every other paragraph's prep. Correctness rests on version discipline —
// every content change bumps `paragraph.version`, and structural rebuilds /
// canonical installs carry surviving versions FORWARD (never reset to 0), so
// a (style, version) pair is never reused for possibly-different content.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ParagraphPrepKey {
  pub(super) paragraph_key: ParagraphCacheKey,
  pub(super) invisibility_mode: bool,
}

// Shaping/work validity inherits the content-keyed `prep_key` (generation-free
// per §act-nine A9.3) and KEEPS `layout_generation`: theme/zoom/width clears
// bump it deliberately, and those inputs are invisible to the per-paragraph key.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ParagraphLayoutWorkKey {
  pub(super) prep_key: ParagraphPrepKey,
  pub(super) width: Pixels,
  pub(super) layout_generation: u64,
}

#[derive(Clone, Debug)]
pub(super) struct ParagraphPrep {
  pub(super) key: ParagraphPrepKey,
  /// Stable identity of the paragraph this prep was built from — the install
  /// gate and validity checks pin the prep to it, so a positional shift can
  /// never install or serve one paragraph's prep for another that happens to
  /// share a (style, version) pair.
  pub(super) paragraph_id: ParagraphId,
  pub(super) paragraph_ix: usize,
  pub(super) paragraph_text: Arc<str>,
  pub(super) layout_runs: Arc<[TextRun]>,
  pub(super) layout_style: ParagraphStyle,
  pub(super) layout_version: u64,
  pub(super) source_len: usize,
  pub(super) wrap_break_ends: Arc<[usize]>,
  pub(super) visible: bool,
}

pub(super) struct ParagraphPrepBatchRequest {
  pub(super) text: Rope,
  pub(super) theme: DocumentTheme,
  pub(super) invisibility_mode: bool,
  pub(super) requested: usize,
  pub(super) paragraphs: Vec<ParagraphPrepSource>,
  pub(super) max_paragraphs: usize,
  pub(super) max_text_bytes: usize,
}

pub(super) struct ParagraphPrepSource {
  paragraph_id: ParagraphId,
  paragraph_ix: usize,
  paragraph: Paragraph,
  byte_range: Range<usize>,
}

pub(super) struct ParagraphPrepBatchResult {
  pub(super) invisibility_mode: bool,
  pub(super) requested: usize,
  pub(super) completed: usize,
  pub(super) text_bytes: usize,
  pub(super) deferred_paragraphs: Vec<usize>,
  pub(super) preps: Vec<ParagraphPrep>,
}

#[hotpath::measure]
pub(super) fn build_paragraph_prep_batch(request: ParagraphPrepBatchRequest) -> ParagraphPrepBatchResult {
  let mut preps = Vec::new();
  let mut text_bytes = 0usize;
  let mut processed_requests = 0usize;
  let limit = request
    .max_paragraphs
    .min(request.paragraphs.len())
    .max(usize::from(!request.paragraphs.is_empty()));

  for (request_ix, source) in request.paragraphs.iter().take(limit).enumerate() {
    processed_requests = request_ix + 1;
    let Some(prep) = build_paragraph_prep_from_parts(
      &request.text,
      &request.theme,
      source.paragraph_id,
      source.paragraph_ix,
      &source.paragraph,
      source.byte_range.clone(),
      request.invisibility_mode,
    ) else {
      continue;
    };
    text_bytes = text_bytes.saturating_add(prep.paragraph_text.len());
    preps.push(prep);
    if text_bytes >= request.max_text_bytes {
      break;
    }
  }
  let deferred_paragraphs = request
    .paragraphs
    .iter()
    .skip(processed_requests)
    .map(|source| source.paragraph_ix)
    .collect::<Vec<_>>();

  ParagraphPrepBatchResult {
    invisibility_mode: request.invisibility_mode,
    requested: request.requested,
    completed: preps.len(),
    text_bytes,
    deferred_paragraphs,
    preps,
  }
}

#[hotpath::measure]
pub(super) fn paragraph_prep_batch_request(
  document: &DocumentProjection,
  invisibility_mode: bool,
  paragraphs: Vec<usize>,
  max_paragraphs: usize,
  max_text_bytes: usize,
) -> ParagraphPrepBatchRequest {
  let requested = paragraphs.len();
  let paragraphs = paragraphs
    .into_iter()
    .filter_map(|paragraph_ix| {
      let paragraph_id = document.ids.paragraph_ids.get(paragraph_ix).copied()?;
      document
        .paragraphs
        .get(paragraph_ix)
        .cloned()
        .map(|paragraph| ParagraphPrepSource {
          paragraph_id,
          paragraph_ix,
          byte_range: paragraph_byte_range(document, paragraph_ix),
          paragraph,
        })
    })
    .collect();
  ParagraphPrepBatchRequest {
    text: document.text.clone(),
    theme: document.theme.clone(),
    invisibility_mode,
    requested,
    paragraphs,
    max_paragraphs,
    max_text_bytes,
  }
}

thread_local! {
  // §act-eleven C4 tripwire: counts full prep BUILDS since the last reset.
  // Equality tests prove the content-keyed caches are CORRECT; this counter
  // proves they are EFFECTIVE — a regression that quietly reverts to
  // whole-viewport re-prep per keystroke (the T8.12 "landed but inert" class)
  // fails the bounded-recompute test instead of surfacing as a perf mystery.
  static PREP_BUILD_COUNT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Prep builds since the last [`reset_prep_build_count`]. Test observability.
#[allow(dead_code, reason = "consumed by the in-crate C4 tripwire tests; unused in non-test builds")]
#[must_use]
pub fn prep_build_count() -> u64 {
  PREP_BUILD_COUNT.with(std::cell::Cell::get)
}

#[allow(dead_code, reason = "consumed by the in-crate C4 tripwire tests; unused in non-test builds")]
pub fn reset_prep_build_count() {
  PREP_BUILD_COUNT.with(|count| count.set(0));
}

#[hotpath::measure]
pub(super) fn build_paragraph_prep_from_parts(
  text: &Rope,
  theme: &DocumentTheme,
  paragraph_id: ParagraphId,
  paragraph_ix: usize,
  paragraph: &Paragraph,
  paragraph_byte_range: Range<usize>,
  invisibility_mode: bool,
) -> Option<ParagraphPrep> {
  PREP_BUILD_COUNT.with(|count| count.set(count.get() + 1));
  let source_len = paragraph_text_len(paragraph);
  let key = ParagraphPrepKey {
    paragraph_key: paragraph_cache_key_for_paragraph(paragraph),
    invisibility_mode,
  };

  if invisibility_mode && !paragraph_style_is_visible_for_theme(theme, paragraph.style) {
    let Some((text, runs)) = projected_visible_paragraph_text_and_runs_from_text(text, theme, paragraph, paragraph_byte_range.clone()) else {
      // A paragraph whose style is hidden and has no visible runs has nothing
      // to project, so it is hidden rather than rendered as a blank line.
      return Some(ParagraphPrep {
        key,
        paragraph_id,
        paragraph_ix,
        paragraph_text: Arc::from(""),
        layout_runs: Arc::from(Vec::<TextRun>::new().into_boxed_slice()),
        layout_style: paragraph.style,
        layout_version: paragraph.version,
        source_len,
        wrap_break_ends: Arc::from(Vec::<usize>::new().into_boxed_slice()),
        visible: false,
      });
    };

    let wrap_break_ends = wrap_break_ends(&text);
    return Some(ParagraphPrep {
      key,
      paragraph_id,
      paragraph_ix,
      paragraph_text: Arc::from(text),
      layout_runs: Arc::from(runs.into_boxed_slice()),
      layout_style: ParagraphStyle::Normal,
      layout_version: paragraph.version.wrapping_add(INVISIBILITY_PROJECTED_VERSION_OFFSET),
      source_len,
      wrap_break_ends: Arc::from(wrap_break_ends.into_boxed_slice()),
      visible: true,
    });
  }

  if invisibility_mode && !paragraph_is_visible_for_theme(theme, paragraph) {
    return Some(ParagraphPrep {
      key,
      paragraph_id,
      paragraph_ix,
      paragraph_text: Arc::from(""),
      layout_runs: Arc::from(Vec::<TextRun>::new().into_boxed_slice()),
      layout_style: paragraph.style,
      layout_version: paragraph.version,
      source_len,
      wrap_break_ends: Arc::from(Vec::<usize>::new().into_boxed_slice()),
      visible: false,
    });
  }

  let text = paragraph_text_from_rope(text, paragraph_byte_range);
  let wrap_break_ends = wrap_break_ends(&text);
  Some(ParagraphPrep {
    key,
    paragraph_id,
    paragraph_ix,
    paragraph_text: Arc::from(text),
    layout_runs: Arc::from(paragraph.runs.clone().into_boxed_slice()),
    layout_style: paragraph.style,
    layout_version: paragraph.version,
    source_len,
    wrap_break_ends: Arc::from(wrap_break_ends.into_boxed_slice()),
    visible: true,
  })
}

#[hotpath::measure]
pub(super) fn build_paragraph_prep(
  document: &DocumentProjection,
  paragraph_ix: usize,
  invisibility_mode: bool,
) -> Option<ParagraphPrep> {
  let paragraph = document.paragraphs.get(paragraph_ix)?;
  let paragraph_id = document.ids.paragraph_ids.get(paragraph_ix).copied()?;
  let paragraph_byte_range = paragraph_byte_range(document, paragraph_ix);
  build_paragraph_prep_from_parts(
    &document.text,
    &document.theme,
    paragraph_id,
    paragraph_ix,
    paragraph,
    paragraph_byte_range,
    invisibility_mode,
  )
}

#[hotpath::measure]
fn paragraph_text_from_rope(text: &Rope, range: Range<usize>) -> String {
  let mut output = String::with_capacity(range.end.saturating_sub(range.start));
  for chunk in text.byte_slice(range).chunks() {
    output.push_str(chunk);
  }
  output
}

#[hotpath::measure]
fn projected_visible_paragraph_text_and_runs_from_text(
  text: &Rope,
  theme: &DocumentTheme,
  paragraph: &Paragraph,
  paragraph_byte_range: Range<usize>,
) -> Option<(String, Vec<TextRun>)> {
  let paragraph_len = paragraph_text_len(paragraph);
  let visible_run_count = paragraph.runs.iter().filter(|run| run.len > 0 && run_is_visible_for_theme(theme, run.styles)).count();
  if visible_run_count == 0 {
    return None;
  }
  let visible_text_len = paragraph
    .runs
    .iter()
    .filter(|run| run_is_visible_for_theme(theme, run.styles))
    .map(|run| run.len)
    .sum::<usize>();
  let mut output = String::with_capacity(visible_text_len.saturating_add(visible_run_count.saturating_sub(1)));
  let mut runs = Vec::with_capacity(visible_run_count.saturating_mul(2).saturating_sub(1));
  let mut byte = 0usize;

  for run in &paragraph.runs {
    let start = byte;
    let end = start + run.len;
    byte = end;
    if start >= end || end > paragraph_len || !run_is_visible_for_theme(theme, run.styles) {
      continue;
    }
    if !output.is_empty() {
      output.push(' ');
      runs.push(TextRun {
        len: 1,
        styles: RunStyles::default(),
      });
    }
    let piece_start = output.len();
    push_rope_text_slice(text, paragraph_byte_range.start + start..paragraph_byte_range.start + end, &mut output);
    let piece_len = output.len().saturating_sub(piece_start);
    if piece_len == 0 {
      continue;
    }
    runs.push(TextRun {
      len: piece_len,
      styles: run.styles,
    });
  }

  (!output.is_empty()).then_some((output, runs))
}

#[hotpath::measure]
fn push_rope_text_slice(text: &Rope, range: Range<usize>, output: &mut String) {
  for chunk in text.byte_slice(range).chunks() {
    output.push_str(chunk);
  }
}

#[cfg(test)]
mod prep_tests {
  use super::*;

  #[hotpath::measure]
  fn input_run(text: &str, styles: RunStyles) -> InputRun {
    InputRun {
      text: text.to_string(),
      styles,
    }
  }

  #[test]
  #[hotpath::measure]
  fn normal_prep_captures_text_runs_and_wrap_breaks() {
    let mut theme = DocumentTheme::default();
    theme.set_invisibility_visible_semantic_style(1);
    theme.set_invisibility_visible_highlight_style(1);
    let document = document_from_input(
      theme,
      vec![InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![input_run("alpha beta/gamma", RunStyles::default())],
      }],
    );

    let prep = build_paragraph_prep(&document, 0, false).expect("paragraph prep");

    assert!(!prep.key.invisibility_mode);
    assert_eq!(prep.paragraph_id, document.ids.paragraph_ids[0]);
    assert_eq!(prep.paragraph_text.as_ref(), "alpha beta/gamma");
    assert_eq!(prep.layout_runs.len(), 1);
    assert!(prep.visible);
    assert!(prep.wrap_break_ends.iter().any(|byte| *byte == "alpha ".len()));
  }

  #[test]
  #[hotpath::measure]
  fn invisibility_prep_projects_visible_runs_from_hidden_custom_paragraphs() {
    let cite = RunStyles {
      semantic: RunSemanticStyle::Custom(1),
      ..RunStyles::default()
    };
    let spoken = RunStyles {
      highlight: Some(HighlightStyle::Custom(1)),
      ..RunStyles::default()
    };
    let mut theme = DocumentTheme::default();
    theme.set_invisibility_visible_semantic_style(1);
    theme.set_invisibility_visible_highlight_style(1);
    let document = document_from_input(
      theme,
      vec![InputParagraph {
        style: ParagraphStyle::Custom(9),
        runs: vec![
          input_run("hidden ", RunStyles::default()),
          input_run("cite", cite),
          input_run(" also-hidden ", RunStyles::default()),
          input_run("spoken", spoken),
        ],
      }],
    );

    let prep = build_paragraph_prep(&document, 0, true).expect("paragraph prep");

    assert!(prep.visible);
    assert_eq!(prep.paragraph_text.as_ref(), "cite spoken");
    assert_eq!(prep.layout_runs.len(), 3);
    assert_eq!(prep.layout_version, document.paragraphs[0].version.wrapping_add(INVISIBILITY_PROJECTED_VERSION_OFFSET));
  }

  #[test]
  #[hotpath::measure]
  fn invisibility_prep_hides_plain_normal_paragraphs() {
    let document = document_from_input(
      DocumentTheme::default(),
      vec![InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![input_run("hidden", RunStyles::default())],
      }],
    );

    let prep = build_paragraph_prep(&document, 0, true).expect("paragraph prep");

    assert!(!prep.visible);
    assert_eq!(prep.paragraph_text.as_ref(), "");
  }

  #[test]
  #[hotpath::measure]
  fn prep_batch_defers_work_after_text_byte_limit() {
    let document = document_from_input(
      DocumentTheme::default(),
      vec![
        InputParagraph {
          style: ParagraphStyle::Normal,
          runs: vec![input_run("alpha", RunStyles::default())],
        },
        InputParagraph {
          style: ParagraphStyle::Normal,
          runs: vec![input_run("beta", RunStyles::default())],
        },
        InputParagraph {
          style: ParagraphStyle::Normal,
          runs: vec![input_run("gamma", RunStyles::default())],
        },
      ],
    );

    let result = build_paragraph_prep_batch(paragraph_prep_batch_request(&document, false, vec![0, 1, 2], 16, 1));

    assert_eq!(result.completed, 1);
    assert_eq!(result.deferred_paragraphs, vec![1, 2]);
  }

  #[test]
  #[hotpath::measure]
  fn prep_uses_offset_index_range_instead_of_stale_paragraph_field() {
    let document = document_from_input(
      DocumentTheme::default(),
      vec![
        InputParagraph {
          style: ParagraphStyle::Normal,
          runs: vec![input_run("alpha", RunStyles::default())],
        },
        InputParagraph {
          style: ParagraphStyle::Normal,
          runs: vec![input_run("Kepe et al. ‘23", RunStyles::default())],
        },
      ],
    );

    let prep = build_paragraph_prep(&document, 1, false).expect("paragraph prep");

    assert_eq!(prep.paragraph_text.as_ref(), "Kepe et al. ‘23");
  }
}
