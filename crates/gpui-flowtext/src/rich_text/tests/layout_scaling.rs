// Class 3 — the render/layout pipeline must stay VIRTUALIZED: layout work per pass is
// bounded by the viewport, not the document size.
//
// The impact-doc field logs showed `render-layout-pass` stalling with `layout_gen` many
// generations behind `edit_gen`, re-laying-out the visible window every frame on a
// ~6000-paragraph document. If virtualization breaks (the editor exact-lays-out every
// paragraph), layout cost grows with document size and the render thread can never catch
// up. These tests drive real layout via `benchmark_paragraph_item_sizes` in a sized window
// and assert the exact-height / prep work is bounded and does not scale with the document.

use super::*;

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

/// Build an editor in a sized window, force a cold layout, and return the pass metrics.
fn cold_layout_metrics(cx: &mut gpui::TestAppContext, paragraphs: usize) -> ItemSizeBenchmarkResult {
  cx.update(|cx| gpui_component::init(cx));
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

/// After a burst of edits, a re-layout must remain incremental — the edit advances
/// `edit_generation`, and the following layout pass must not re-prep the whole document
/// (the field stall was the layout redoing work every frame while falling behind edits).
#[gpui::test]
fn relayout_after_edits_stays_incremental(cx: &mut gpui::TestAppContext) {
  let paragraphs = 3000;
  cx.update(|cx| gpui_component::init(cx));
  let handle = cx.add_window(|_window, cx| RichTextEditor::new_with_path(big_document(paragraphs), None, cx));
  handle
    .update(cx, |editor, window, cx| {
      // Warm the layout once.
      let _ = editor.benchmark_paragraph_item_sizes(px(760.0), window, cx);

      // Type several graphemes near the top; each advances the edit generation.
      editor.selection = EditorSelection::collapsed(DocumentOffset { paragraph: 1, byte: 0 });
      let gen_before = editor.edit_generation();
      for _ in 0..8 {
        editor.insert_single_grapheme_fast_path("x", cx);
      }
      assert!(editor.edit_generation() > gen_before, "edits must advance the edit generation");

      // A re-layout after the edits must prep only a bounded number of chunks, not the doc.
      let metrics = editor.benchmark_paragraph_item_sizes(px(760.0), window, cx);
      assert!(
        metrics.prep_installed < paragraphs / 3,
        "re-layout after {} edits re-prepped {} of {paragraphs} chunks — not incremental",
        8,
        metrics.prep_installed,
      );
    })
    .expect("windowed edit + relayout");
}
