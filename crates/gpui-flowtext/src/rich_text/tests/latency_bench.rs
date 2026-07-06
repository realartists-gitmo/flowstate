// Keystroke-echo latency guard for the "maximally fast" mission: measures the
// editor-side optimistic mutation primitives on a large document. Run manually:
//   cargo test -p gpui-flowtext --release -- --ignored keystroke_latency

#[test]
#[ignore = "latency benchmark; run manually with --release --ignored"]
fn keystroke_latency_editor_echo_benchmark() {
  let paragraphs = (0..5_000)
    .map(|ix| InputParagraph {
      style: if ix % 40 == 0 { ParagraphStyle::Custom(2) } else { ParagraphStyle::Normal },
      runs: vec![plain(
        "Competitive policy debate rewards evidence organized for instant retrieval and flawless formatting.",
      )],
    })
    .collect();
  let mut document = document_from_input(DocumentTheme::default(), paragraphs);
  let target = document.paragraphs.len() / 2;

  let mut samples = Vec::with_capacity(2_500);
  let mut byte = 0usize;
  for step in 0..2_000 {
    let started = std::time::Instant::now();
    insert_text_at(&mut document, target, byte, "x", RunStyles::default());
    samples.push(started.elapsed());
    byte += 1;
    if step % 400 == 399 {
      let started = std::time::Instant::now();
      delete_range_in_paragraph(&mut document, target, 0..400);
      samples.push(started.elapsed());
      byte -= 400;
    }
  }

  samples.sort();
  let p50 = samples[samples.len() / 2];
  let p99 = samples[samples.len() * 99 / 100];
  let max = *samples.last().unwrap();
  println!(
    "keystroke echo latency over {} samples on a {}-paragraph document: p50={p50:?} p99={p99:?} max={max:?}",
    samples.len(),
    document.paragraphs.len(),
  );
  // Generous ceiling: catches catastrophic regressions (accidental O(document)
  // work per keystroke) without flaking on slow CI machines.
  assert!(p99 < std::time::Duration::from_millis(25), "keystroke echo p99 regressed: {p99:?}");
}
