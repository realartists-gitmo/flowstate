#[hotpath::measure]
fn benchmark_layout_paths(
  document: &DocumentProjection,
  widths: &[f32],
  iterations: usize,
  include_paint: bool,
  window: &mut Window,
  cx: &mut Context<BenchmarkRunner>,
) -> Vec<LayoutBenchRow> {
  let mut rows = Vec::new();
  for width in widths {
    let width_px = px(*width);
    let estimate_all = repeated(iterations, || {
      let mut total = 0.0f32;
      for paragraph_ix in 0..document.paragraphs.len() {
        let height: f32 = estimate_paragraph_item_height(document, paragraph_ix, width_px).into();
        total += height;
      }
      std::hint::black_box(total);
    });
    let visibility_visible = repeated(iterations, || {
      std::hint::black_box(VisibilityIndex::build(document, false));
    });
    let visibility_invisible = repeated(iterations, || {
      std::hint::black_box(VisibilityIndex::build(document, true));
    });

    let mut full_layout = None;
    let full_layout_duration = repeated(iterations, || {
      full_layout = Some(build_layout(document, width_px, None, window, cx));
    });
    let layout = full_layout.expect("full layout benchmark should produce a layout");
    let reuse_layout_duration = repeated(iterations, || {
      std::hint::black_box(build_layout(document, width_px, Some(&layout), window, cx));
    });
    let structural_layout = repeated(iterations, || {
      std::hint::black_box(build_structural_block_layout(document, width_px, None, window, cx));
    });

    let paint_bounds = Bounds::new(point(px(0.0), px(0.0)), size(width_px, layout.size.height));
    let paint_plain = include_paint.then(|| {
      repeated(iterations, || {
        paint_layout(
          &layout,
          paint_bounds,
          None,
          None,
          false,
          px(1.0),
          None,
          gpui::black(),
          &[],
          &[],
          &[],
          &[],
          None,
          window,
          cx,
        );
      })
    });
    let selection = top_selection(document);
    let paint_selected = include_paint.then(|| {
      repeated(iterations, || {
        paint_layout(
          &layout,
          paint_bounds,
          selection.as_ref(),
          None,
          false,
          px(1.0),
          None,
          gpui::black(),
          &[],
          &[],
          &[],
          &[],
          None,
          window,
          cx,
        );
      })
    });

    let editor = cx.new(|cx| RichTextEditor::new_with_path(document.clone(), None, cx));
    let item_sizes_cold = editor.update(cx, |editor, cx| {
      editor.benchmark_invalidate_document_layout_caches();
      editor.benchmark_paragraph_item_sizes(width_px, window, cx)
    });
    let item_sizes_hot = editor.update(cx, |editor, cx| editor.benchmark_paragraph_item_sizes(width_px, window, cx));
    let item_sizes_invisible = editor.update(cx, |editor, cx| {
      editor.set_invisibility_mode(true, cx);
      editor.benchmark_paragraph_item_sizes(width_px, window, cx)
    });

    let (estimate_mean_abs_error, estimate_max_abs_error) = estimate_error(document, &layout, width_px);
    let summary = summarize_layout(document, &layout);
    rows.push(LayoutBenchRow {
      width: *width,
      estimate_all,
      visibility_visible,
      visibility_invisible,
      full_layout: full_layout_duration,
      reuse_layout: reuse_layout_duration,
      structural_layout,
      paint_plain,
      paint_selected,
      item_sizes_cold,
      item_sizes_hot,
      item_sizes_invisible,
      estimate_mean_abs_error,
      estimate_max_abs_error,
      summary,
    });
  }
  rows
}

#[hotpath::measure]
fn benchmark_sample_paragraph_layouts(
  document: &DocumentProjection,
  stats: &DocumentStats,
  widths: &[f32],
  iterations: usize,
  window: &mut Window,
  cx: &mut Context<BenchmarkRunner>,
) -> Vec<ParagraphLayoutRow> {
  let mut rows = Vec::new();
  if document.paragraphs.is_empty() {
    return rows;
  }
  let mut samples = vec![
    ("first".to_string(), 0usize),
    ("middle".to_string(), document.paragraphs.len() / 2),
    ("last".to_string(), document.paragraphs.len().saturating_sub(1)),
    ("largest".to_string(), stats.largest_paragraph_ix),
    ("most_runs".to_string(), stats.most_runs_paragraph_ix),
  ];
  samples.sort_by_key(|(_, ix)| *ix);
  samples.dedup_by_key(|(_, ix)| *ix);

  for width in widths {
    let width_px = px(*width);
    for (label, paragraph_ix) in &samples {
      let paragraph_ix = (*paragraph_ix).min(document.paragraphs.len().saturating_sub(1));
      let mut normal_layout = None;
      let normal = repeated(iterations, || {
        normal_layout = Some(build_single_paragraph_layout_with_visibility(
          document,
          paragraph_ix,
          width_px,
          None,
          false,
          window,
          cx,
        ));
      });
      let mut invisible_layout = None;
      let invisible = repeated(iterations, || {
        invisible_layout = Some(build_single_paragraph_layout_with_visibility(
          document,
          paragraph_ix,
          width_px,
          None,
          true,
          window,
          cx,
        ));
      });
      let normal_layout = normal_layout.expect("single paragraph layout benchmark should produce a layout");
      let invisible_layout = invisible_layout.expect("single paragraph invisible layout benchmark should produce a layout");
      let summary = summarize_layout(document, &normal_layout);
      rows.push(ParagraphLayoutRow {
        label: label.clone(),
        paragraph_ix,
        width: *width,
        normal,
        invisible,
        lines: summary.lines,
        segments: summary.segments,
        normal_height: px_to_f32(normal_layout.size.height),
        invisible_height: px_to_f32(invisible_layout.size.height),
      });
    }
  }

  rows
}

#[hotpath::measure]
fn repeated(iterations: usize, mut run: impl FnMut()) -> DurationStats {
  let mut timings = Vec::with_capacity(iterations.max(1));
  for _ in 0..iterations.max(1) {
    let started = Instant::now();
    run();
    timings.push(started.elapsed());
  }
  DurationStats::from_samples(&timings)
}

#[hotpath::measure]
fn estimate_error(document: &DocumentProjection, layout: &LayoutState, width: Pixels) -> (f32, f32) {
  let mut total = 0.0f32;
  let mut max = 0.0f32;
  let mut count = 0usize;
  for (layout_ix, paragraph) in layout.paragraphs.iter().enumerate() {
    let exact = if let Some(next) = layout.paragraphs.get(layout_ix + 1) {
      px_to_f32(next.top - paragraph.top)
    } else {
      px_to_f32(layout.size.height - paragraph.top)
    };
    let estimate = px_to_f32(estimate_paragraph_item_height(document, paragraph.index, width));
    let error = (estimate - exact).abs();
    total += error;
    max = max.max(error);
    count += 1;
  }
  if count == 0 { (0.0, 0.0) } else { (total / count as f32, max) }
}

#[hotpath::measure]
fn summarize_layout(document: &DocumentProjection, layout: &LayoutState) -> LayoutSummary {
  let mut summary = LayoutSummary {
    layout_height: px_to_f32(layout.size.height),
    ..Default::default()
  };
  for paragraph in &layout.paragraphs {
    if paragraph.index >= document.paragraphs.len() {
      summary.fidelity_failures += 1;
    }
    let mut previous_bottom = px(-1.0);
    for line in &paragraph.lines {
      summary.lines += 1;
      summary.segments += line.segments.len();
      summary.rects += line.rects.len();
      summary.underlines += line.underlines.len();
      summary.strikethroughs += line.strikethroughs.len();
      summary.max_line_width = summary.max_line_width.max(px_to_f32(line.width));
      if line.start_byte > line.end_byte || line.end_byte > paragraph.len || line.origin.y < previous_bottom {
        summary.fidelity_failures += 1;
      }
      previous_bottom = line.origin.y + line.line_height;
      for segment in &line.segments {
        if segment.start_byte > line.end_byte {
          summary.fidelity_failures += 1;
        }
      }
    }
  }
  summary
}
