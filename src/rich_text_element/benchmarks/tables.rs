fn write_operation_table(out: &mut String, title: &str, rows: &[OperationRow]) {
  let _ = writeln!(out, "### {title}");
  let _ = writeln!(out);
  let _ = writeln!(out, "| benchmark | min ms | mean ms | max ms | samples | fidelity failures |");
  let _ = writeln!(out, "|---|---:|---:|---:|---:|---:|");
  for row in rows {
    let _ = writeln!(
      out,
      "| {} | {:.3} | {:.3} | {:.3} | {} | {} |",
      md(&row.name),
      ms(row.duration.min),
      ms(row.duration.mean),
      ms(row.duration.max),
      row.duration.samples,
      row.fidelity_failures
    );
  }
  let _ = writeln!(out);
}

fn write_layout_table(out: &mut String, rows: &[LayoutBenchRow]) {
  let _ = writeln!(out, "### Layout, Paint, And Virtual List Benchmarks");
  let _ = writeln!(out);
  let _ = writeln!(
    out,
    "| width | estimate all mean ms | full layout mean ms | reused layout mean ms | structural mean ms | paint mean ms | selected paint mean ms | item sizes cold/hot/invis ms | lines | segments | height | estimate mean/max abs error px | fidelity failures |"
  );
  let _ = writeln!(out, "|---:|---:|---:|---:|---:|---:|---:|---|---:|---:|---:|---:|---:|");
  for row in rows {
    let paint = row
      .paint_plain
      .map(|stats| format!("{:.3}", ms(stats.mean)))
      .unwrap_or_else(|| "n/a".to_string());
    let selected_paint = row
      .paint_selected
      .map(|stats| format!("{:.3}", ms(stats.mean)))
      .unwrap_or_else(|| "n/a".to_string());
    let _ = writeln!(
      out,
      "| {:.0} | {:.3} | {:.3} | {:.3} | {:.3} | {} | {} | {:.3}/{:.3}/{:.3} | {} | {} | {:.1} | {:.1}/{:.1} | {} |",
      row.width,
      ms(row.estimate_all.mean),
      ms(row.full_layout.mean),
      ms(row.reuse_layout.mean),
      ms(row.structural_layout.mean),
      paint,
      selected_paint,
      ms(row.item_sizes_cold.elapsed),
      ms(row.item_sizes_hot.elapsed),
      ms(row.item_sizes_invisible.elapsed),
      row.summary.lines,
      row.summary.segments,
      row.summary.layout_height,
      row.estimate_mean_abs_error,
      row.estimate_max_abs_error,
      row.summary.fidelity_failures
    );
  }
  let _ = writeln!(out);

  let _ = writeln!(out, "Item size cache detail:");
  let _ = writeln!(out);
  let _ = writeln!(
    out,
    "| width | cold hit | hot hit | invis hit | items | exact heights cold/hot/invis | total height cold/hot/invis | prep req/done/inst/stale | prep batches/KB | UI chunks/ms | budget overruns prefetch/scroll | visibility visible/invisible mean ms |"
  );
  let _ = writeln!(out, "|---:|---|---|---|---:|---|---|---|---|---|---|---:|");
  for row in rows {
    let prep_requested = row.item_sizes_cold.prep_requested + row.item_sizes_hot.prep_requested + row.item_sizes_invisible.prep_requested;
    let prep_completed = row.item_sizes_cold.prep_completed + row.item_sizes_hot.prep_completed + row.item_sizes_invisible.prep_completed;
    let prep_installed = row.item_sizes_cold.prep_installed + row.item_sizes_hot.prep_installed + row.item_sizes_invisible.prep_installed;
    let prep_stale = row.item_sizes_cold.prep_stale + row.item_sizes_hot.prep_stale + row.item_sizes_invisible.prep_stale;
    let prep_batches = row.item_sizes_cold.prep_batches + row.item_sizes_hot.prep_batches + row.item_sizes_invisible.prep_batches;
    let prep_bytes = row.item_sizes_cold.prep_text_bytes + row.item_sizes_hot.prep_text_bytes + row.item_sizes_invisible.prep_text_bytes;
    let ui_chunk_builds = row.item_sizes_cold.ui_chunk_builds + row.item_sizes_hot.ui_chunk_builds + row.item_sizes_invisible.ui_chunk_builds;
    let ui_chunk_time = row.item_sizes_cold.ui_chunk_build_time + row.item_sizes_hot.ui_chunk_build_time + row.item_sizes_invisible.ui_chunk_build_time;
    let prefetch_overruns =
      row.item_sizes_cold.prefetch_budget_overruns + row.item_sizes_hot.prefetch_budget_overruns + row.item_sizes_invisible.prefetch_budget_overruns;
    let scroll_overruns =
      row.item_sizes_cold.scroll_budget_overruns + row.item_sizes_hot.scroll_budget_overruns + row.item_sizes_invisible.scroll_budget_overruns;
    let _ = writeln!(
      out,
      "| {:.0} | {} | {} | {} | {} | {}/{}/{} | {:.1}/{:.1}/{:.1} | {}/{}/{}/{} | {}/{:.1} | {}/{:.3} | {}/{} | {:.3}/{:.3} |",
      row.width,
      row.item_sizes_cold.cache_hit,
      row.item_sizes_hot.cache_hit,
      row.item_sizes_invisible.cache_hit,
      row.item_sizes_cold.item_count,
      row.item_sizes_cold.exact_height_count,
      row.item_sizes_hot.exact_height_count,
      row.item_sizes_invisible.exact_height_count,
      row.item_sizes_cold.total_height,
      row.item_sizes_hot.total_height,
      row.item_sizes_invisible.total_height,
      prep_requested,
      prep_completed,
      prep_installed,
      prep_stale,
      prep_batches,
      prep_bytes as f32 / 1024.0,
      ui_chunk_builds,
      ms(ui_chunk_time),
      prefetch_overruns,
      scroll_overruns,
      ms(row.visibility_visible.mean),
      ms(row.visibility_invisible.mean)
    );
  }
  let _ = writeln!(out);
}

fn write_paragraph_layout_table(out: &mut String, rows: &[ParagraphLayoutRow]) {
  let _ = writeln!(out, "### Sample Paragraph Layout Benchmarks");
  let _ = writeln!(out);
  let _ = writeln!(
    out,
    "| sample | paragraph | width | normal mean ms | invisibility mean ms | normal height | invisibility height | lines | segments |"
  );
  let _ = writeln!(out, "|---|---:|---:|---:|---:|---:|---:|---:|---:|");
  for row in rows {
    let _ = writeln!(
      out,
      "| {} | {} | {:.0} | {:.3} | {:.3} | {:.1} | {:.1} | {} | {} |",
      md(&row.label),
      row.paragraph_ix,
      row.width,
      ms(row.normal.mean),
      ms(row.invisible.mean),
      row.normal_height,
      row.invisible_height,
      row.lines,
      row.segments
    );
  }
  let _ = writeln!(out);
}

