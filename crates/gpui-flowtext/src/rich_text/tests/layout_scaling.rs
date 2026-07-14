// Class 3 — the render/layout pipeline must stay VIRTUALIZED: layout work per pass is
// bounded by the viewport, not the document size.
//
// The impact-doc field logs showed `render-layout-pass` stalling with `layout_gen` many
// generations behind `edit_gen`, re-laying-out the visible window every frame on a
// ~6000-paragraph document. If virtualization breaks (the editor exact-lays-out every
// paragraph), layout cost grows with document size and the render thread can never catch
// up. These tests drive real layout via `benchmark_paragraph_item_sizes` in a sized window
// and assert the exact-height / prep work is bounded and does not scale with the document.

fn big_document(paragraphs: usize) -> DocumentProjection {
  let paras = (0..paragraphs)
    .map(|ix| InputParagraph {
      style: if ix % 40 == 0 { ParagraphStyle::Custom(2) } else { ParagraphStyle::Normal },
      runs: vec![plain(
        "Body paragraph carrying several words so the layout has real text to measure and reflow.",
      )],
    })
    .collect();
  document_from_input(DocumentTheme::default(), paras)
}

/// §perf-heaven T7.16: content the ASCII `big_document` never exercises — a CJK
/// paragraph (multibyte: byte length ≫ char count), a very-long-word paragraph
/// (word wrap can't break inside it), a code-like line, and an empty paragraph.
/// This is what guards the T7.17 byte-vs-char estimate fix: a multibyte estimate
/// measured in bytes would over-shoot the exact height ~3–4×, tripping the net's
/// over-shoot bound.
fn varied_document(repeats: usize) -> DocumentProjection {
  // The first sample is a LONG pure-CJK paragraph (every char is 3 UTF-8 bytes,
  // and it wraps over many lines) so a byte-length estimate over-shoots the exact
  // height by ~3× — decisively past the net's 2× over-shoot bound. The shorter
  // samples alone stay under it (the base +1 line dampens a one-liner), so this
  // long paragraph is what makes the net actually TRIP on a T7.17 regression.
  let long_cjk = "日本語の長い段落を用意して、文字数とバイト数の差が高さ推定に与える影響を確かめます。".repeat(6);
  let samples: [String; 6] = [
    long_cjk,
    "Supercalifragilisticexpialidocious-antidisestablishmentarianism-pneumonoultramicroscopicsilicovolcanoconiosis".to_string(),
    "let chars_per_line = ((content_width / avg_char_width * WORD_WRAP_FILL).floor() as usize).max(1);".to_string(),
    "Ordinary body paragraph carrying several words so the layout has real text to measure and reflow across lines.".to_string(),
    "Mixed 混合 content with ASCII and 日本語 together on one line to stress the char-vs-byte estimate boundary.".to_string(),
    String::new(),
  ];
  let paras = (0..repeats)
    .flat_map(|_| {
      samples.iter().map(|text| InputParagraph {
        style: ParagraphStyle::Normal,
        runs: if text.is_empty() { Vec::new() } else { vec![plain(text)] },
      })
    })
    .collect();
  document_from_input(DocumentTheme::default(), paras)
}

/// Build an editor in a sized window, force a cold layout, and return the pass metrics.
fn cold_layout_metrics(cx: &mut gpui::TestAppContext, paragraphs: usize) -> ItemSizeBenchmarkResult {
  cx.update(gpui_component::init);
  let handle = cx.add_window(|_window, cx| RichTextEditor::new_with_path(big_document(paragraphs), None, cx));
  handle
    .update(cx, |editor, window, cx| {
      editor.benchmark_invalidate_document_layout_caches();
      editor.benchmark_paragraph_item_sizes(px(760.0), window, cx)
    })
    .expect("windowed layout pass")
}

/// A cold layout of a 3000-paragraph document must exact-lay-out far fewer paragraphs than
/// the document holds — the rest are virtualized with estimated heights.
#[gpui::test]
fn cold_layout_exact_heights_are_far_fewer_than_the_document(cx: &mut gpui::TestAppContext) {
  let paragraphs = 3000;
  let metrics = cold_layout_metrics(cx, paragraphs);
  assert!(
    metrics.exact_height_count < paragraphs / 3,
    "cold layout exact-laid-out {} of {paragraphs} paragraphs — virtualization is not bounding layout to the viewport",
    metrics.exact_height_count,
  );
  assert!(
    metrics.prep_installed < paragraphs / 3,
    "cold layout prepped {} of {paragraphs} paragraph chunks — layout prep is not virtualized",
    metrics.prep_installed,
  );
}

/// Tripling the document size with the SAME viewport must not multiply the exact-height /
/// prep work — layout stays O(viewport), not O(document). A virtualization regression that
/// lays out every paragraph would blow the exact-height count up ~3×.
#[gpui::test]
fn layout_exact_height_work_does_not_scale_with_document_size(cx: &mut gpui::TestAppContext) {
  let small = cold_layout_metrics(cx, 2000);
  let large = cold_layout_metrics(cx, 6000);

  let exact_ceiling = small.exact_height_count.max(64) * 2;
  assert!(
    large.exact_height_count <= exact_ceiling,
    "exact-height layout scaled with document size (virtualization regression): 2000-para={} vs 6000-para={} (ceiling {exact_ceiling})",
    small.exact_height_count,
    large.exact_height_count,
  );
  let prep_ceiling = small.prep_installed.max(64) * 2;
  assert!(
    large.prep_installed <= prep_ceiling,
    "layout prep scaled with document size: 2000-para={} vs 6000-para={} (ceiling {prep_ceiling})",
    small.prep_installed,
    large.prep_installed,
  );
}

/// §perf-heaven T5 NET (the layout-fidelity oracle): the per-paragraph height
/// ESTIMATE must track the exact height within a bounded band. Under-shooting is
/// the dangerous case — scroll jumps UP as real heights land below the caret — so
/// it is bounded tightly; over-shooting only wastes space. Layout heights are not
/// covered by the CRDT convergence fuzz or the corpus sweep, so THIS is the net
/// that guards the estimate heuristic and any future persisted-estimate work
/// (a persisted estimate that diverges from a fresh one trips here).
#[gpui::test]
fn estimate_tracks_exact_height_within_bounds(cx: &mut gpui::TestAppContext) {
  cx.update(gpui_component::init);
  let handle = cx.add_window(|_window, cx| RichTextEditor::new_with_path(big_document(400), None, cx));
  let acc = handle
    .update(cx, |editor, window, cx| {
      editor.benchmark_invalidate_document_layout_caches();
      editor.benchmark_estimate_accuracy(px(760.0), window, cx)
    })
    .expect("windowed layout pass");
  eprintln!(
    "T5 estimate-accuracy net: compared={} max_over={:.3} max_under={:.3}",
    acc.compared, acc.max_over_ratio, acc.max_under_ratio,
  );
  assert!(acc.compared >= 8, "net must compare real laid-out paragraphs (else it's vacuous), got {}", acc.compared);
  assert!(
    acc.max_under_ratio < 0.2,
    "estimate UNDER-shot exact height by >20% — scroll would jump up (T5 heuristic/word-wrap regression): {:.3}",
    acc.max_under_ratio,
  );
  assert!(
    acc.max_over_ratio < 2.0,
    "estimate OVER-shot exact height by >2x (T5 heuristic regression): {:.3}",
    acc.max_over_ratio,
  );
}

/// §perf-heaven T7.16/T7.17 NET: the estimate must stay within the same bounds
/// on VARIED content — crucially multibyte (CJK) paragraphs, where measuring the
/// text in bytes instead of characters over-shoots the exact height ~3–4×. With
/// the byte-length estimate this trips the over-shoot bound; with the char-count
/// fix it holds.
#[gpui::test]
fn estimate_tracks_exact_height_for_varied_content(cx: &mut gpui::TestAppContext) {
  cx.update(gpui_component::init);
  let handle = cx.add_window(|_window, cx| RichTextEditor::new_with_path(varied_document(70), None, cx));
  let acc = handle
    .update(cx, |editor, window, cx| {
      editor.benchmark_invalidate_document_layout_caches();
      editor.benchmark_estimate_accuracy(px(760.0), window, cx)
    })
    .expect("windowed layout pass");
  eprintln!(
    "T7.16 varied-content estimate-accuracy net: compared={} max_over={:.3} max_under={:.3}",
    acc.compared, acc.max_over_ratio, acc.max_under_ratio,
  );
  assert!(acc.compared >= 8, "net must compare real laid-out paragraphs (else it's vacuous), got {}", acc.compared);
  assert!(
    acc.max_under_ratio < 0.2,
    "varied-content estimate UNDER-shot exact height by >20% (scroll would jump up): {:.3}",
    acc.max_under_ratio,
  );
  // Tighter than the general 2.0 bound: on this multibyte-heavy fixture the
  // char-count estimate (T7.17) holds max_over ≈ 0, while the byte-length
  // regression pushes it to ≈1.27 (the Latin-calibrated `avg_char_width` only
  // partially absorbs the 3-bytes-per-CJK-char inflation, so it never reaches
  // 2×). A 1.0 bound cleanly separates the two — this is the assertion that
  // actually TRIPS on a T7.17 regression, proven by reverting `chars().count()`
  // to `.len()` (→ max_over ≈ 1.27, fails).
  assert!(
    acc.max_over_ratio < 1.0,
    "varied-content estimate OVER-shot exact height by >1x — T7.17 byte-vs-char regression on multibyte text: {:.3}",
    acc.max_over_ratio,
  );
}

/// G22 measurement — time the cold layout + shape pass (the CPU side of scroll:
/// `paint_line_text`/`shape_fragment`) at increasing document sizes. Run with
/// `--nocapture`. Because layout is viewport-virtualized, the pass time should stay
/// roughly FLAT as the document grows; a rising number is a virtualization regression.
#[gpui::test]
fn g22_cold_layout_pass_timing(cx: &mut gpui::TestAppContext) {
  cx.update(gpui_component::init);
  for paragraphs in [2000_usize, 6000, 12000] {
    let handle = cx.add_window(|_window, cx| RichTextEditor::new_with_path(big_document(paragraphs), None, cx));
    let (ms, exact, prep) = handle
      .update(cx, |editor, window, cx| {
        editor.benchmark_invalidate_document_layout_caches();
        let started = std::time::Instant::now();
        let metrics = editor.benchmark_paragraph_item_sizes(px(760.0), window, cx);
        (started.elapsed().as_secs_f64() * 1000.0, metrics.exact_height_count, metrics.prep_installed)
      })
      .expect("windowed layout pass");
    eprintln!("G22 cold-layout {paragraphs:>6} paras: {ms:8.2} ms  (exact_heights={exact}, prep={prep})");
    assert!(exact < paragraphs, "layout must stay virtualized (exact-laid {exact} of {paragraphs})");
  }
}

