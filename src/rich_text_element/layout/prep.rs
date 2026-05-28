use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ParagraphPrepKey {
  pub(super) paragraph_key: ParagraphCacheKey,
  pub(super) invisibility_mode: bool,
  pub(super) edit_generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ParagraphLayoutWorkKey {
  pub(super) prep_key: ParagraphPrepKey,
  pub(super) width: Pixels,
  pub(super) layout_generation: u64,
}

#[derive(Clone, Debug)]
pub(super) struct ParagraphPrep {
  pub(super) key: ParagraphPrepKey,
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
  pub(super) document: Document,
  pub(super) edit_generation: u64,
  pub(super) invisibility_mode: bool,
  pub(super) paragraphs: Vec<usize>,
  pub(super) max_paragraphs: usize,
  pub(super) max_text_bytes: usize,
}

pub(super) struct ParagraphPrepBatchResult {
  pub(super) edit_generation: u64,
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

  for (request_ix, paragraph_ix) in request.paragraphs.iter().copied().take(limit).enumerate() {
    processed_requests = request_ix + 1;
    let Some(prep) = build_paragraph_prep(
      &request.document,
      paragraph_ix,
      request.edit_generation,
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
    .copied()
    .skip(processed_requests)
    .collect::<Vec<_>>();

  ParagraphPrepBatchResult {
    edit_generation: request.edit_generation,
    invisibility_mode: request.invisibility_mode,
    requested: request.paragraphs.len(),
    completed: preps.len(),
    text_bytes,
    deferred_paragraphs,
    preps,
  }
}

#[hotpath::measure]
pub(super) fn build_paragraph_prep(
  document: &Document,
  paragraph_ix: usize,
  edit_generation: u64,
  invisibility_mode: bool,
) -> Option<ParagraphPrep> {
  let paragraph = document.paragraphs.get(paragraph_ix)?;
  let source_len = paragraph_text_len(paragraph);
  let key = ParagraphPrepKey {
    paragraph_key: paragraph_cache_key(document, paragraph),
    invisibility_mode,
    edit_generation,
  };

  if invisibility_mode && !paragraph_is_visible(paragraph) {
    return Some(ParagraphPrep {
      key,
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

  if invisibility_mode && matches!(paragraph.style, ParagraphStyle::Normal) {
    let source = paragraph_text(document, paragraph_ix);
    let mut byte = 0usize;
    let mut text = String::new();
    let mut runs = Vec::new();

    for run in &paragraph.runs {
      let start = byte;
      let end = start + run.len;
      byte = end;
      if !run_is_visible(run.styles) {
        continue;
      }
      let piece = source.get(start..end).unwrap_or("");
      if piece.is_empty() {
        continue;
      }
      if !text.is_empty() {
        text.push(' ');
        runs.push(TextRun {
          len: 1,
          styles: RunStyles::default(),
        });
      }
      text.push_str(piece);
      runs.push(TextRun {
        len: piece.len(),
        styles: run.styles,
      });
    }

    if text.is_empty() {
      let source_wrap_break_ends = wrap_break_ends(&source);
      return Some(ParagraphPrep {
        key,
        paragraph_ix,
        paragraph_text: Arc::from(source),
        layout_runs: Arc::from(paragraph.runs.clone().into_boxed_slice()),
        layout_style: paragraph.style,
        layout_version: paragraph.version,
        source_len,
        wrap_break_ends: Arc::from(source_wrap_break_ends.into_boxed_slice()),
        visible: true,
      });
    }

    let wrap_break_ends = wrap_break_ends(&text);
    return Some(ParagraphPrep {
      key,
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

  let text = paragraph_text(document, paragraph_ix);
  let wrap_break_ends = wrap_break_ends(&text);
  Some(ParagraphPrep {
    key,
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
    let document = document_from_input(
      DocumentTheme::default(),
      vec![InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![input_run("alpha beta/gamma", RunStyles::default())],
      }],
    );

    let prep = build_paragraph_prep(&document, 0, 7, false).expect("paragraph prep");

    assert_eq!(prep.key.edit_generation, 7);
    assert_eq!(prep.paragraph_text.as_ref(), "alpha beta/gamma");
    assert_eq!(prep.layout_runs.len(), 1);
    assert!(prep.visible);
    assert!(prep.wrap_break_ends.iter().any(|byte| *byte == "alpha ".len()));
  }

  #[test]
  #[hotpath::measure]
  fn invisibility_prep_projects_visible_runs() {
    let cite = RunStyles {
      semantic: RunSemanticStyle::Cite,
      ..RunStyles::default()
    };
    let spoken = RunStyles {
      highlight: Some(HighlightStyle::Spoken),
      ..RunStyles::default()
    };
    let document = document_from_input(
      DocumentTheme::default(),
      vec![InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![
          input_run("hidden ", RunStyles::default()),
          input_run("cite", cite),
          input_run(" also-hidden ", RunStyles::default()),
          input_run("spoken", spoken),
        ],
      }],
    );

    let prep = build_paragraph_prep(&document, 0, 3, true).expect("paragraph prep");

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

    let prep = build_paragraph_prep(&document, 0, 1, true).expect("paragraph prep");

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

    let result = build_paragraph_prep_batch(ParagraphPrepBatchRequest {
      document,
      edit_generation: 9,
      invisibility_mode: false,
      paragraphs: vec![0, 1, 2],
      max_paragraphs: 16,
      max_text_bytes: 1,
    });

    assert_eq!(result.completed, 1);
    assert_eq!(result.deferred_paragraphs, vec![1, 2]);
  }
}
