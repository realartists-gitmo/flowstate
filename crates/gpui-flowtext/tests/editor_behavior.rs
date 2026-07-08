//! Mutating editor-behavior tests (Loro-first spec §5/§13).
//!
//! These live in an INTEGRATION target (not the unit suites) so the real
//! flowstate-collab write authority and the editor share one gpui-flowtext
//! crate instance — invariant 5's "one write path" holds even in tests. An
//! editor without an authority is a read-only display surface; every test
//! here attaches `LocalDocHandle` over a canonical Loro core imported from
//! its fixture, exactly like production document open.

#[cfg(test)]
mod tests {
  use flowstate_collab::crdt_runtime::CrdtRuntime;
  use flowstate_collab::local_write::{LocalDocHandle, LocalWriteConfig};
  use gpui::AppContext as _;
  use gpui::px;
  use gpui_flowtext::*;

  /// Import the editor's fixture projection into a canonical core and install
  /// the REAL write authority (production wiring).
  fn attach_test_authority(editor: &gpui::Entity<RichTextEditor>, cx: &mut gpui::TestAppContext) {
    let fixture = editor.read_with(cx, |editor, _| editor.document().clone());
    let core = CrdtRuntime::from_document_projection(&fixture, "Test Document")
      .expect("importing the fixture projection into a canonical Loro core");
    let (handle, _gate) = LocalDocHandle::new(core, LocalWriteConfig::default());
    let projection = handle
      .projection()
      .expect("reading the authority's canonical projection");
    editor.update(cx, |editor, cx| {
      editor.set_write_authority(std::sync::Arc::new(handle), projection, cx);
    });
  }

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

  fn editor_with_authority(document: DocumentProjection, cx: &mut gpui::TestAppContext) -> gpui::Entity<RichTextEditor> {
    let editor = cx.new(|cx| RichTextEditor::new_with_path(document, None, cx));
    attach_test_authority(&editor, cx);
    editor
  }

  /// Text edits through the REAL write path (editor command → typed intent →
  /// write authority → canonical Loro commit → exact projection patches):
  /// inserts, minimal marks over the changed range, and range deletes preserve
  /// text and run structure. (Formerly a unit test of the retired optimistic
  /// projection-edit helpers.)
  #[gpui::test]
  fn text_edits_preserve_text_and_styles_through_the_authority(cx: &mut gpui::TestAppContext) {
    let emphasized = RunStyles::default().with(RunStyle::Semantic(2));
    let editor = editor_with_authority(
      document_from_input(
        DocumentTheme::default(),
        vec![InputParagraph {
          style: ParagraphStyle::Normal,
          runs: vec![run("hello", RunStyles::default())],
        }],
      ),
      cx,
    );

    editor.update(cx, |editor, cx| {
      editor.set_selection(EditorSelection::collapsed(DocumentOffset {
        paragraph: 0,
        byte: "he".len(),
      }), cx);
      editor.insert_text_command("y", cx);
      assert_eq!(paragraph_text(editor.document(), 0), "heyllo");
      assert_eq!(editor.document().paragraphs[0].runs.len(), 1);

      editor.set_selection(EditorSelection::range(
        DocumentOffset {
          paragraph: 0,
          byte: "hey".len(),
        },
        DocumentOffset {
          paragraph: 0,
          byte: "heyll".len(),
        },
      ), cx);
      editor.apply_run_style_to_selection(RunStyle::Semantic(2), cx);
      assert_eq!(paragraph_text(editor.document(), 0), "heyllo");
      assert_eq!(editor.document().paragraphs[0].runs.len(), 3);
      assert_eq!(editor.document().paragraphs[0].runs[1].styles, emphasized);

      editor.set_selection(EditorSelection::range(
        DocumentOffset {
          paragraph: 0,
          byte: "he".len(),
        },
        DocumentOffset {
          paragraph: 0,
          byte: "heyll".len(),
        },
      ), cx);
      editor.backspace_command(cx);
      assert_eq!(paragraph_text(editor.document(), 0), "heo");
      assert_eq!(editor.document().paragraphs[0].runs.len(), 1);
      assert_eq!(editor.document().paragraphs[0].runs[0].styles, RunStyles::default());
    });
  }


  /// §act-three C (background open): a panel painted from a phase-V cached
  /// projection is a READ-ONLY display surface (no authority) — edits are inert
  /// — until phase G attaches the authority, after which editing commits
  /// normally and the projection is the authority's canonical one. This is the
  /// editor-level heart of the two-phase open (`create_pending_document_panel`
  /// → `attach_runtime_to_pending_panel`).
  #[gpui::test]
  fn read_only_panel_becomes_editable_when_authority_attaches(cx: &mut gpui::TestAppContext) {
    let projection = document_from_input(
      DocumentTheme::default(),
      vec![InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![run("cached view", RunStyles::default())],
      }],
    );
    // Phase V: editor built from the cached projection, NO authority.
    let editor = cx.new(|cx| RichTextEditor::new_with_path(projection.clone(), None, cx));

    editor.update(cx, |editor, cx| {
      assert!(!editor.has_write_authority(), "phase-V panel must have no write authority");
      // An edit against a read-only surface is inert (no fallback editing).
      editor.set_selection(EditorSelection::collapsed(DocumentOffset { paragraph: 0, byte: "cached".len() }), cx);
      editor.insert_text_command("X", cx);
      assert_eq!(paragraph_text(editor.document(), 0), "cached view", "read-only surface must ignore edits");
    });

    // Phase G: the authority runtime finishes loading and attaches.
    attach_test_authority(&editor, cx);

    editor.update(cx, |editor, cx| {
      assert!(editor.has_write_authority(), "phase-G attach must install the write authority");
      // The projection is now the authority's canonical one (same content).
      assert_eq!(paragraph_text(editor.document(), 0), "cached view");
      // Editing now commits through the real write path.
      editor.set_selection(EditorSelection::collapsed(DocumentOffset { paragraph: 0, byte: "cached".len() }), cx);
      editor.insert_text_command("X", cx);
      assert_eq!(paragraph_text(editor.document(), 0), "cachedX view", "editing must work after attach");
    });
  }

  /// UTF-8 byte-offset law through the real write path: text anchors address
  /// paragraph bytes while the canonical Loro body is unicode — inserts and
  /// deletes around multi-byte characters must round-trip exactly.
  #[gpui::test]
  fn document_rope_edits_keep_utf8_byte_offsets(cx: &mut gpui::TestAppContext) {
    let editor = editor_with_authority(
      document_from_input(
        DocumentTheme::default(),
        vec![InputParagraph {
          style: ParagraphStyle::Normal,
          runs: vec![run("abé🚀cd", RunStyles::default())],
        }],
      ),
      cx,
    );

    editor.update(cx, |editor, cx| {
      editor.set_selection(EditorSelection::collapsed(DocumentOffset {
        paragraph: 0,
        byte: "abé".len(),
      }), cx);
      editor.insert_text_command("Z", cx);
      assert_eq!(paragraph_text(editor.document(), 0), "abéZ🚀cd");

      editor.set_selection(EditorSelection::range(
        DocumentOffset {
          paragraph: 0,
          byte: "abé".len(),
        },
        DocumentOffset {
          paragraph: 0,
          byte: "abéZ🚀".len(),
        },
      ), cx);
      editor.backspace_command(cx);
      assert_eq!(paragraph_text(editor.document(), 0), "abécd");
    });
  }


  /// Rich paste through the canonical `InsertRichFragment` intent: the FIRST
  /// pasted paragraph flows into the caret paragraph, which keeps ITS OWN
  /// paragraph style (the destination wins — the canonical fragment law; the
  /// retired optimistic paste adopted the pasted style instead), while each
  /// subsequent pasted paragraph carries its own style across the new boundary.
  #[gpui::test]
  fn rich_paste_keeps_destination_style_and_carries_subsequent_paragraph_styles(cx: &mut gpui::TestAppContext) {
    let editor = editor_with_authority(
      document_from_input(
        DocumentTheme::default(),
        vec![InputParagraph {
          style: ParagraphStyle::Normal,
          runs: Vec::new(),
        }],
      ),
      cx,
    );
    let fragment = RichClipboardFragment {
      format: RICH_TEXT_CLIPBOARD_FORMAT.to_string(),
      paragraphs: vec![
        InputParagraph {
          style: ParagraphStyle::Custom(1),
          runs: vec![plain("Heading")],
        },
        InputParagraph {
          style: ParagraphStyle::Custom(3),
          runs: vec![plain("Tag")],
        },
      ],
      blocks: Vec::new(),
      assets: Vec::new(),
    };

    editor.update(cx, |editor, cx| {
      editor.set_selection(EditorSelection::collapsed(DocumentOffset::default()), cx);
      assert!(editor.insert_rich_fragment_paste_at_caret(&fragment, cx));

      assert_eq!(editor.document().paragraphs.len(), 2);
      assert_eq!(paragraph_text(editor.document(), 0), "Heading");
      // Destination paragraph style wins for the first pasted paragraph.
      assert_eq!(editor.document().paragraphs[0].style, ParagraphStyle::Normal);
      assert_eq!(paragraph_text(editor.document(), 1), "Tag");
      assert_eq!(editor.document().paragraphs[1].style, ParagraphStyle::Custom(3));
    });
  }


  /// Clear-formatting over a selection spanning an empty paragraph, through the
  /// REAL write path: whole-paragraph selections clear paragraph styles back to
  /// Normal plus one run-style reset over the span (the editor's
  /// `clear_formatting` policy, committed via the write authority).
  #[gpui::test]
  fn selection_across_empty_paragraphs_and_clear_formatting_policy(cx: &mut gpui::TestAppContext) {
    let emphasized = RunStyles::default().with(RunStyle::Semantic(2));
    let editor = editor_with_authority(
      document_from_input(
        DocumentTheme::default(),
        vec![
          InputParagraph {
            style: ParagraphStyle::Custom(3),
            runs: vec![run("tag", emphasized)],
          },
          InputParagraph {
            style: ParagraphStyle::Custom(0),
            runs: Vec::new(),
          },
          InputParagraph {
            style: ParagraphStyle::Normal,
            runs: vec![run("body", emphasized)],
          },
        ],
      ),
      cx,
    );

    editor.update(cx, |editor, cx| {
      let selection = DocumentOffset { paragraph: 0, byte: 1 }..DocumentOffset { paragraph: 2, byte: 1 };
      assert!(selection_contains_whole_paragraph(editor.document(), selection.clone()));

      editor.set_selection(EditorSelection::range(selection.start, selection.end), cx);
      editor.clear_formatting(cx);

      for paragraph in &editor.document().paragraphs {
        assert_eq!(paragraph.style, ParagraphStyle::Normal);
        assert!(
          paragraph
            .runs
            .iter()
            .all(|run| run.styles == RunStyles::default())
        );
      }
    });
  }


  /// Soft line breaks through the REAL write path (one `InsertText` intent —
  /// U+2028 is body text, not a paragraph boundary): the paragraph must not
  /// split, and plain-text copy renders the break as '\n'.
  #[gpui::test]
  fn soft_line_break_stays_inside_paragraph_and_copies_as_newline(cx: &mut gpui::TestAppContext) {
    let editor = editor_with_authority(
      document_from_input(
        DocumentTheme::default(),
        vec![InputParagraph {
          style: ParagraphStyle::Normal,
          runs: vec![plain("alphaomega")],
        }],
      ),
      cx,
    );

    editor.update(cx, |editor, cx| {
      editor.set_selection(EditorSelection::collapsed(DocumentOffset {
        paragraph: 0,
        byte: "alpha".len(),
      }), cx);
      editor.insert_text_command(SOFT_LINE_BREAK_STR, cx);

      assert_eq!(editor.document().paragraphs.len(), 1);
      assert_eq!(paragraph_text(editor.document(), 0), format!("alpha{SOFT_LINE_BREAK_STR}omega"));
      assert_eq!(
        selected_plain_text(
          editor.document(),
          DocumentOffset { paragraph: 0, byte: 0 }..DocumentOffset {
            paragraph: 0,
            byte: paragraph_text_len(&editor.document().paragraphs[0]),
          },
        ),
        "alpha\nomega"
      );
    });
  }


  /// Cross-paragraph run-style mutation through the REAL write path (one
  /// `SetMarks` intent over the selection): only the selected span changes;
  /// runs and unselected text on both sides stay intact.
  #[gpui::test]
  fn cross_paragraph_style_mutation_keeps_runs_and_unselected_text_intact(cx: &mut gpui::TestAppContext) {
    let editor = editor_with_authority(
      document_from_input(
        DocumentTheme::default(),
        vec![
          InputParagraph {
            style: ParagraphStyle::Normal,
            runs: vec![plain("abc")],
          },
          InputParagraph {
            style: ParagraphStyle::Normal,
            runs: vec![plain("def")],
          },
        ],
      ),
      cx,
    );

    editor.update(cx, |editor, cx| {
      editor.set_selection(EditorSelection::range(
        DocumentOffset { paragraph: 0, byte: 1 },
        DocumentOffset { paragraph: 1, byte: 2 },
      ), cx);
      editor.apply_run_style_to_selection(RunStyle::Semantic(1), cx);

      let document = editor.document();
      assert_eq!(paragraph_text(document, 0), "abc");
      assert_eq!(paragraph_text(document, 1), "def");
      assert_ne!(document.paragraphs[0].runs[0].styles.semantic, RunSemanticStyle::Custom(1));
      assert_eq!(document.paragraphs[0].runs[1].styles.semantic, RunSemanticStyle::Custom(1));
      assert_eq!(document.paragraphs[1].runs[0].styles.semantic, RunSemanticStyle::Custom(1));
      assert_ne!(document.paragraphs[1].runs[1].styles.semantic, RunSemanticStyle::Custom(1));
    });
  }

  /// After a burst of edits, a re-layout must remain incremental — the edit advances
  /// `edit_generation`, and the following layout pass must not re-prep the whole document
  /// (the field stall was the layout redoing work every frame while falling behind edits).
  #[gpui::test]
  fn relayout_after_edits_stays_incremental(cx: &mut gpui::TestAppContext) {
    let paragraphs = 3000;
    cx.update(gpui_component::init);
    let handle = cx.add_window(|_window, cx| RichTextEditor::new_with_path(big_document(paragraphs), None, cx));
    // Edits only commit through a write authority (invariant 5) — attach the
    // real one before typing.
    let editor = handle.root(cx).expect("window root editor");
    attach_test_authority(&editor, cx);
    handle
      .update(cx, |editor, window, cx| {
        // Warm the layout once.
        let _ = editor.benchmark_paragraph_item_sizes(px(760.0), window, cx);

        // Type several graphemes near the top; each advances the edit generation.
        editor.set_selection(EditorSelection::collapsed(DocumentOffset { paragraph: 1, byte: 0 }), cx);
        let gen_before = editor.edit_generation();
        for _ in 0..8 {
          editor.insert_text_command("x", cx);
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

  /// Spec §11 keystroke-latency ratchet (initial ceilings: p50 < 16ms,
  /// p95 < 50ms on the large fixture). Measures the FULL Loro-first path per
  /// keystroke — intent build → write gate → identity resolution → Loro
  /// commit → exact patch synthesis → patch apply into THE projection — on a
  /// 5000-paragraph document. Latency is now a UI-thread input-latency
  /// guarantee, not an actor round-trip. Run manually:
  ///   `cargo test -p gpui-flowtext --release --test editor_behavior -- --ignored`
  #[gpui::test]
  #[ignore = "latency ratchet; run manually with --release --ignored"]
  fn keystroke_latency_through_the_authority(cx: &mut gpui::TestAppContext) {
    let paragraphs = (0..5_000)
      .map(|ix| InputParagraph {
        style: if ix % 40 == 0 { ParagraphStyle::Custom(2) } else { ParagraphStyle::Normal },
        runs: vec![plain(
          "Competitive policy debate rewards evidence organized for instant retrieval and flawless formatting.",
        )],
      })
      .collect();
    let editor = editor_with_authority(document_from_input(DocumentTheme::default(), paragraphs), cx);
    let target = editor.read_with(cx, |editor, _| editor.document().paragraphs.len() / 2);
    editor.update(cx, |editor, cx| {
      editor.set_selection(EditorSelection::collapsed(DocumentOffset { paragraph: target, byte: 0 }), cx);
    });

    let mut samples = Vec::with_capacity(1_800);
    editor.update(cx, |editor, cx| {
      for step in 0..1_500 {
        let started = std::time::Instant::now();
        editor.insert_text_command("x", cx);
        samples.push(started.elapsed());
        if step % 5 == 4 {
          let started = std::time::Instant::now();
          editor.backspace_command(cx);
          samples.push(started.elapsed());
        }
      }
    });

    samples.sort();
    let p50 = samples[samples.len() / 2];
    let p95 = samples[samples.len() * 95 / 100];
    let p99 = samples[samples.len() * 99 / 100];
    let max = *samples.last().expect("samples recorded");
    println!(
      "Loro-first keystroke latency over {} samples on a 5000-paragraph document: p50={p50:?} p95={p95:?} p99={p99:?} max={max:?}",
      samples.len(),
    );
    // Spec §11 initial ratchet ceilings — tighten on improvement, never loosen
    // without a human decision. Being under budget is not a quality claim.
    assert!(p50 < std::time::Duration::from_millis(16), "keystroke p50 regressed past the ratchet: {p50:?}");
    assert!(p95 < std::time::Duration::from_millis(50), "keystroke p95 regressed past the ratchet: {p95:?}");
  }

  /// Replace-all must reach CANONICAL state (2026-07-07 field class: the old
  /// implementation rewrote the editor's projection copy directly for
  /// same-paragraph matches, so saves, peers, and undo silently lost every
  /// replacement — the projection looked right until the next authority
  /// emission clobbered it).
  #[gpui::test]
  fn replace_all_search_highlights_commits_canonically(cx: &mut gpui::TestAppContext) {
    let editor = cx.new(|cx| {
      RichTextEditor::new_with_path(
        document_from_input(
          DocumentTheme::default(),
          vec![
            InputParagraph {
              style: ParagraphStyle::Normal,
              runs: vec![plain("alpha foo beta foo")],
            },
            InputParagraph {
              style: ParagraphStyle::Normal,
              runs: vec![plain("foo gamma")],
            },
          ],
        ),
        None,
        cx,
      )
    });
    let fixture = editor.read_with(cx, |editor, _| editor.document().clone());
    let core = CrdtRuntime::from_document_projection(&fixture, "Replace").expect("canonical core");
    let (handle, _gate) = LocalDocHandle::new(core, LocalWriteConfig::default());
    let handle = std::sync::Arc::new(handle);
    let projection = handle.projection().expect("canonical projection");
    editor.update(cx, |editor, cx| {
      editor.set_write_authority(handle.clone(), projection, cx);
    });

    editor.update(cx, |editor, cx| {
      let ranges = vec![
        DocumentOffset { paragraph: 0, byte: 6 }..DocumentOffset { paragraph: 0, byte: 9 },
        DocumentOffset { paragraph: 0, byte: 15 }..DocumentOffset { paragraph: 0, byte: 18 },
        DocumentOffset { paragraph: 1, byte: 0 }..DocumentOffset { paragraph: 1, byte: 3 },
      ];
      editor.set_search_highlights(ranges, Some(0), cx);
      assert_eq!(editor.replace_all_search_highlights("bar", cx), 3);
    });

    // THE data-loss assertion: the write authority's canonical state carries
    // every replacement.
    let canonical = handle.projection().expect("canonical after replace");
    assert_eq!(paragraph_text(&canonical, 0), "alpha bar beta bar");
    assert_eq!(paragraph_text(&canonical, 1), "bar gamma");
    // The editor's projection agrees with canonical (no divergent local copy).
    editor.read_with(cx, |editor, _| {
      assert_eq!(paragraph_text(editor.document(), 0), "alpha bar beta bar");
      assert_eq!(paragraph_text(editor.document(), 1), "bar gamma");
    });

    // One undo reverts the whole replace-all (the compound intent is one
    // undo member), and the revert reaches canonical too.
    editor.update(cx, |editor, cx| {
      editor.undo(cx);
    });
    let canonical = handle.projection().expect("canonical after undo");
    assert_eq!(paragraph_text(&canonical, 0), "alpha foo beta foo");
    assert_eq!(paragraph_text(&canonical, 1), "foo gamma");
  }
}
