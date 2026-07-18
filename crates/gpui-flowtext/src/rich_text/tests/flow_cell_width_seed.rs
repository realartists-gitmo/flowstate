// The flow grid measures a cell's height from whatever its content element
// last painted, and the autofit row is the tallest cell's measured height. A
// cell has TWO content paths: idle it renders through `RichTextDocumentElement`
// at the true column width; focused it renders through a freshly-built
// `RichTextEditor`. A fresh editor has never been laid out, so
// `current_layout_width` / `paragraph_item_sizes` fall back to `px(900.0)` — far
// wider than a real column — and a multi-line cell wraps to FEWER lines and
// reports a SHORTER height on the first focus frame. The autofit row then
// collapses and every cell below shifts, with a transient re-wrap flash as the
// real width lands a frame later.
//
// `seed_layout_width` is the fix: the grid seeds the new editor with the cell's
// real content-box width so its first frame wraps identically to the idle
// display path. These tests pin that the fallback path really does mis-measure
// (so the net isn't vacuous) and that seeding restores the true-width height.

/// A paragraph whose text wraps to several lines in a narrow debate column but
/// collapses toward one line at the 900px unmeasured fallback — the exact shape
/// that makes a fresh editor mis-measure a cell's height.
fn wrapping_cell_document() -> DocumentProjection {
  document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain(
        "Extinction outweighs on magnitude and timeframe because the impact is \
         irreversible and terminal — no later round of debate can recover a dead \
         planet, so you evaluate it first under any framework.",
      )],
    }],
  )
}

/// Build a flow-cell editor (the same config the grid installs) in a windowed
/// test so real text shaping runs.
fn flow_cell_editor(cx: &mut gpui::TestAppContext) -> gpui::WindowHandle<RichTextEditor> {
  cx.update(gpui_component::init);
  cx.add_window(|_window, cx| {
    let mut editor = RichTextEditor::new_with_path(wrapping_cell_document(), None, cx);
    editor.update_config(
      |config| {
        config.allow_paragraph_breaks = false;
        config.flow_cell_surface = true;
        config.show_section_collapse_controls = false;
      },
      cx,
    );
    editor
  })
}

/// The bug: an unseeded flow-cell editor lays its text out WIDER than the real
/// column (the `px(900.0)` fallback in the app; the full window width here) — so
/// a multi-line cell wraps to fewer lines and reports a SHORTER height than it
/// does at its true column width. That shorter height is what makes focus
/// collapse the autofit row and shift the column.
#[gpui::test]
fn fresh_flow_cell_editor_undermeasures_without_a_width_seed(cx: &mut gpui::TestAppContext) {
  let handle = flow_cell_editor(cx);
  // A real 1AC-ish column: ~280px wide, minus the cell chrome ≈ 262px.
  let narrow = px(262.0);

  let (unseeded_height, seeded_height, unseeded_width) = handle
    .update(cx, |editor, window, cx| {
      // The state of a cell editor before the grid constrains it to the column:
      // whatever width it fell back to, NOT the cell's content width.
      let unseeded_width = editor.benchmark_measured_item_width();
      let unseeded_height = editor.benchmark_flow_cell_height(window, cx);
      // Now seed the true content width, as `ensure_cell_editor` does.
      editor.seed_layout_width(narrow, cx);
      let seeded_height = editor.benchmark_flow_cell_height(window, cx);
      (unseeded_height, seeded_height, unseeded_width)
    })
    .expect("window update");

  assert!(
    unseeded_width.is_none_or(|width| width > narrow),
    "an unseeded cell editor lays out WIDER than the real column ({unseeded_width:?} vs {narrow:?}) — \
     that (or the 900px fallback in the app) is why it under-measures the cell's height"
  );
  assert!(
    seeded_height > unseeded_height + px(1.0),
    "the too-wide fallback under-measured the cell: unseeded={unseeded_height:?} vs seeded-at-column-width={seeded_height:?}. \
     A focused cell that reports this shorter height collapses the autofit row and shifts the column."
  );
}

/// The fix: seeding the editor with the cell's real content width makes it wrap
/// exactly as if it had been measured at that width — so entering a cell is a
/// no-op on height and there is no shift or re-wrap flash.
#[gpui::test]
fn seeded_flow_cell_height_matches_a_real_measurement_at_that_width(cx: &mut gpui::TestAppContext) {
  let handle = flow_cell_editor(cx);
  let narrow = px(262.0);

  let (seeded_height, measured_reference, measured_width) = handle
    .update(cx, |editor, window, cx| {
      editor.seed_layout_width(narrow, cx);
      let seeded_height = editor.benchmark_flow_cell_height(window, cx);
      let measured_width = editor.benchmark_measured_item_width();
      // `benchmark_paragraph_item_sizes` forces a real layout at `narrow` and
      // reports its total height — the ground truth the seed must reproduce.
      editor.benchmark_invalidate_document_layout_caches();
      let reference = editor.benchmark_paragraph_item_sizes(narrow, window, cx);
      // The focused editor's `flow_cell_height` is the raw content height with
      // NO base pad (Fix #1) — exactly what the idle display path reports and
      // what this reference sums. Any pad here would nudge the row on focus.
      let measured_reference = px(reference.total_height);
      (seeded_height, measured_reference, measured_width)
    })
    .expect("window update");

  assert_eq!(
    measured_width,
    Some(narrow),
    "seeding must set the editor's layout width so its first frame wraps at the true column width"
  );
  assert_eq!(
    seeded_height, measured_reference,
    "a seeded cell editor must lay out at EXACTLY the height a real measurement at that width produces \
     (no base pad) — otherwise focus changes the row height"
  );
}
