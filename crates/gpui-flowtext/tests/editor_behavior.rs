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
    let core =
      CrdtRuntime::from_document_projection(&fixture, "Test Document").expect("importing the fixture projection into a canonical Loro core");
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
        style: if ix % 40 == 0 {
          ParagraphStyle::Custom(2)
        } else {
          ParagraphStyle::Normal
        },
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

  /// §act-eleven C9: the editor interaction smoke-fuzz — a seeded random mix
  /// over the PUBLIC command surface (typing, backspace/delete, word deletes,
  /// paragraph breaks, selection moves, undo/redo) through the REAL write
  /// authority. Invariants after EVERY op: id vectors parallel to their
  /// sequences, selection within bounds on a char boundary, the rope's byte
  /// length consistent with the paragraph lengths, and the editor's document
  /// text equal to the authority's canonical projection text (no silent
  /// editor/authority drift). The command layer had NO randomized coverage —
  /// the collab fuzzes drive the model beneath it, not these entry points.
  #[gpui::test]
  fn interaction_smoke_fuzz_preserves_editor_invariants(cx: &mut gpui::TestAppContext) {
    struct Rng(u64);
    impl Rng {
      fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
      }
      fn below(&mut self, bound: usize) -> usize {
        if bound == 0 { 0 } else { (self.next() % bound as u64) as usize }
      }
    }

    for seed in [11u64, 2026, 424242] {
      let mut rng = Rng(seed);
      // Attach the authority by hand so the test keeps the handle for the
      // editor-vs-canonical drift assertion.
      let editor = cx.new(|cx| RichTextEditor::new_with_path(big_document(12), None, cx));
      let fixture = editor.read_with(cx, |editor, _| editor.document().clone());
      let core = CrdtRuntime::from_document_projection(&fixture, "Smoke Fuzz").expect("canonical core");
      let (handle, _gate) = LocalDocHandle::new(core, LocalWriteConfig::default());
      let authority = std::sync::Arc::new(handle);
      let projection = authority.projection().expect("authority projection");
      editor.update(cx, |editor, cx| {
        #[allow(
          clippy::clone_on_ref_ptr,
          reason = "method-syntax clone so the unsized coercion applies to the annotated binding"
        )]
        let authority_dyn: std::sync::Arc<dyn LocalWriteAuthority> = authority.clone();
        editor.set_write_authority(authority_dyn, projection, cx);
      });
      editor.update(cx, |editor, cx| {
        for step in 0..160 {
          let paragraph_count = editor.document().paragraphs.len();
          let paragraph_ix = rng.below(paragraph_count.max(1));
          let paragraph_len = flowstate_document::paragraph_text_len(&editor.document().paragraphs[paragraph_ix]);
          let byte = clamp_paragraph_byte_to_char_boundary(editor.document(), paragraph_ix, rng.below(paragraph_len + 1));
          let offset = DocumentOffset {
            paragraph: paragraph_ix,
            byte,
          };
          match rng.below(10) {
            0..=2 => {
              editor.set_selection(EditorSelection::collapsed(offset), cx);
              editor.insert_text_command(&format!("s{step} "), cx);
            },
            3 => {
              editor.set_selection(EditorSelection::collapsed(offset), cx);
              editor.backspace_command(cx);
            },
            4 => {
              editor.set_selection(EditorSelection::collapsed(offset), cx);
              editor.delete_forward_command(cx);
            },
            5 => {
              editor.set_selection(EditorSelection::collapsed(offset), cx);
              editor.delete_word_backward_command(cx);
            },
            6 => {
              editor.set_selection(EditorSelection::collapsed(offset), cx);
              editor.delete_word_forward_command(cx);
            },
            7 => {
              editor.set_selection(EditorSelection::collapsed(offset), cx);
              editor.insert_paragraph_break_command(cx);
            },
            8 => editor.undo(cx),
            _ => editor.redo(cx),
          }

          // ---- invariants, every step ----
          let document = editor.document();
          assert!(!document.paragraphs.is_empty(), "seed {seed} step {step}: document emptied");
          assert_eq!(
            document.ids.paragraph_ids.len(),
            document.paragraphs.len(),
            "seed {seed} step {step}: paragraph id vector desynced"
          );
          assert_eq!(
            document.ids.block_ids.len(),
            document.blocks.len(),
            "seed {seed} step {step}: block id vector desynced"
          );
          let expected_rope_len: usize = document
            .paragraphs
            .iter()
            .map(flowstate_document::paragraph_text_len)
            .sum::<usize>()
            + document.paragraphs.len().saturating_sub(1);
          assert_eq!(
            document.text.byte_len(),
            expected_rope_len,
            "seed {seed} step {step}: rope length inconsistent with paragraph lengths"
          );
          let head = editor.selection().head;
          assert!(
            head.paragraph < document.paragraphs.len(),
            "seed {seed} step {step}: selection paragraph out of bounds"
          );
          let head_paragraph_len = flowstate_document::paragraph_text_len(&document.paragraphs[head.paragraph]);
          assert!(head.byte <= head_paragraph_len, "seed {seed} step {step}: selection byte out of bounds");
          let canonical = authority.projection().expect("authority projection");
          assert_eq!(
            document.text.to_string(),
            canonical.text.to_string(),
            "seed {seed} step {step}: editor text drifted from the authority's canonical projection"
          );
        }
      });
    }
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
      editor.set_selection(
        EditorSelection::collapsed(DocumentOffset {
          paragraph: 0,
          byte: "he".len(),
        }),
        cx,
      );
      editor.insert_text_command("y", cx);
      assert_eq!(paragraph_text(editor.document(), 0), "heyllo");
      assert_eq!(editor.document().paragraphs[0].runs.len(), 1);

      editor.set_selection(
        EditorSelection::range(
          DocumentOffset {
            paragraph: 0,
            byte: "hey".len(),
          },
          DocumentOffset {
            paragraph: 0,
            byte: "heyll".len(),
          },
        ),
        cx,
      );
      editor.apply_run_style_to_selection(RunStyle::Semantic(2), cx);
      assert_eq!(paragraph_text(editor.document(), 0), "heyllo");
      assert_eq!(editor.document().paragraphs[0].runs.len(), 3);
      assert_eq!(editor.document().paragraphs[0].runs[1].styles, emphasized);

      editor.set_selection(
        EditorSelection::range(
          DocumentOffset {
            paragraph: 0,
            byte: "he".len(),
          },
          DocumentOffset {
            paragraph: 0,
            byte: "heyll".len(),
          },
        ),
        cx,
      );
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
      editor.set_selection(
        EditorSelection::collapsed(DocumentOffset {
          paragraph: 0,
          byte: "cached".len(),
        }),
        cx,
      );
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
      editor.set_selection(
        EditorSelection::collapsed(DocumentOffset {
          paragraph: 0,
          byte: "cached".len(),
        }),
        cx,
      );
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
      editor.set_selection(
        EditorSelection::collapsed(DocumentOffset {
          paragraph: 0,
          byte: "abé".len(),
        }),
        cx,
      );
      editor.insert_text_command("Z", cx);
      assert_eq!(paragraph_text(editor.document(), 0), "abéZ🚀cd");

      editor.set_selection(
        EditorSelection::range(
          DocumentOffset {
            paragraph: 0,
            byte: "abé".len(),
          },
          DocumentOffset {
            paragraph: 0,
            byte: "abéZ🚀".len(),
          },
        ),
        cx,
      );
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
      editor.set_selection(
        EditorSelection::collapsed(DocumentOffset {
          paragraph: 0,
          byte: "alpha".len(),
        }),
        cx,
      );
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
      editor.set_selection(
        EditorSelection::range(DocumentOffset { paragraph: 0, byte: 1 }, DocumentOffset { paragraph: 1, byte: 2 }),
        cx,
      );
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
        style: if ix % 40 == 0 {
          ParagraphStyle::Custom(2)
        } else {
          ParagraphStyle::Normal
        },
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
    assert!(
      p50 < std::time::Duration::from_millis(16),
      "keystroke p50 regressed past the ratchet: {p50:?}"
    );
    assert!(
      p95 < std::time::Duration::from_millis(50),
      "keystroke p95 regressed past the ratchet: {p95:?}"
    );
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

  /// FAIL-LOUD (concurrent-typing interleave): when a remote peer inserts text
  /// BEFORE the local caret, the caret must stay pinned to its own content — not
  /// merely clamp to document bounds. Today `apply_remote_patch_batch` /
  /// `sync_projection_from_authority` only clamp `self.selection.head`, never
  /// shifting it by the inserted length, so the local user's next keystroke
  /// resolves to a stale projection offset and interleaves into the remote run.
  ///
  /// Deterministic (no Fugue tie-break dependence): peer B prepends before the
  /// SHARED base character "X", so B's run is unambiguously ordered before A's
  /// caret. The edit reaches A through the REAL authority + projection stream.
  #[gpui::test]
  fn remote_insert_before_caret_must_shift_not_interleave(cx: &mut gpui::TestAppContext) {
    use flowstate_collab::crdt_runtime::CrdtRuntime;
    use flowstate_collab::local_write::GateHolder;

    // Shared base: one paragraph "XY". Build A's canonical core, snapshot it so
    // peer B can share A's exact history, then install the real authority.
    let fixture = document_from_input(
      DocumentTheme::default(),
      vec![InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("XY")],
      }],
    );
    let core_a = CrdtRuntime::from_document_projection(&fixture, "peer-A").expect("core A");
    // Peer B is a fork of A's doc: it shares A's exact history but has a distinct
    // peer id, so its edits are genuinely concurrent and merge cleanly.
    let doc_b = core_a.doc().fork();
    doc_b
      .set_peer_id(0x00B0_B0B0)
      .expect("B gets a distinct peer id"); // fork keeps A's id
    let base_vv = doc_b.state_vv();
    let body_b = flowstate_document::loro_schema::body_text(&doc_b);

    let (handle_a, gate_a) = LocalDocHandle::new(core_a, LocalWriteConfig::default());
    let projection_a = handle_a.projection().expect("A projection");
    let editor = cx.new(|cx| RichTextEditor::new_with_path(projection_a.clone(), None, cx));
    editor.update(cx, |editor, cx| {
      editor.set_write_authority(std::sync::Arc::new(handle_a), projection_a, cx);
    });

    // B inserts "ABC" at the START of paragraph 0 (body position 1, just after the
    // leading boundary "\n"), then exports ONLY its new ops (delta from the shared
    // base version vector).
    body_b.insert(1, "ABC").expect("B inserts");
    doc_b.commit();
    let update_b = doc_b
      .export(loro::ExportMode::updates(&base_vv))
      .expect("B exports its delta");

    // A parks its caret between the shared X and Y ("X|Y" → byte 1).
    editor.update(cx, |editor, cx| {
      editor.set_selection(EditorSelection::collapsed(DocumentOffset { paragraph: 0, byte: 1 }), cx);
    });

    // B's edit arrives at A through the production path: gate import → ordered
    // projection stream → editor drain. Release the gate before the editor drains
    // (its sync re-locks the same gate).
    let mut guard = gate_a.lock(GateHolder::ImportChunk).expect("gate healthy");
    guard.import_remote_update(&update_b).expect("A imports B");
    drop(guard);
    editor.update(cx, |editor, cx| editor.sync_projection_from_authority(cx));

    editor.read_with(cx, |editor, _| {
      assert_eq!(
        paragraph_text(editor.document(), 0),
        "ABCXY",
        "B's prepend must merge before the shared text"
      );
      // Caret was at "X|Y" (byte 1). Three bytes inserted BEFORE it ⇒ the same
      // logical spot is byte 4 ("ABCX|Y"). A bare clamp leaves it at byte 1
      // ("A|BCXY"), which is the interleave bug.
      assert_eq!(
        editor.selection().head,
        DocumentOffset { paragraph: 0, byte: 4 },
        "caret must SHIFT past a remote insert-before-caret, not merely clamp"
      );
    });

    // The symptom itself: A types 'H'. It must land after its own X → "ABCXHY",
    // never interleaved into B's run ("ABHCXY").
    editor.update(cx, |editor, cx| editor.insert_text_command("H", cx));
    editor.read_with(cx, |editor, _| {
      assert_eq!(
        paragraph_text(editor.document(), 0),
        "ABCXHY",
        "A's keystroke must not interleave into the remote run"
      );
    });
  }

  /// FAST-path guard: once the caret has been captured at a synced moment (here by
  /// a local edit), a remote insert-before-caret repositions it by RESOLVING the
  /// stored CRDT cursors — it must NOT fall back to the O(doc) `fork_at` rebase.
  /// Asserts both correctness (caret + no interleave) and that ZERO forks happened,
  /// so a regression to always-fork on every remote batch fails loudly.
  #[gpui::test]
  fn armed_caret_uses_fast_cursor_path_not_fork(cx: &mut gpui::TestAppContext) {
    use flowstate_collab::crdt_runtime::{CrdtRuntime, caret_rebase_fork_count};
    use flowstate_collab::local_write::GateHolder;

    let fixture = document_from_input(
      DocumentTheme::default(),
      vec![InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("XY")],
      }],
    );
    let core_a = CrdtRuntime::from_document_projection(&fixture, "peer-A").expect("core A");
    // B forks the ORIGINAL base (before A's local edit), so B's insert is concurrent.
    let doc_b = core_a.doc().fork();
    doc_b.set_peer_id(0x00B0_B0B0).expect("distinct peer id");
    let base_vv = doc_b.state_vv();
    let body_b = flowstate_document::loro_schema::body_text(&doc_b);

    let (handle_a, gate_a) = LocalDocHandle::new(core_a, LocalWriteConfig::default());
    let projection_a = handle_a.projection().expect("A projection");
    let editor = cx.new(|cx| RichTextEditor::new_with_path(projection_a.clone(), None, cx));
    editor.update(cx, |editor, cx| {
      editor.set_write_authority(std::sync::Arc::new(handle_a), projection_a, cx);
    });

    // A performs a LOCAL edit ("Z" between X and Y): "XY" → "XZY", caret after Z
    // (byte 2). This ARMS the fast-path anchor for the post-write caret.
    editor.update(cx, |editor, cx| {
      editor.set_selection(EditorSelection::collapsed(DocumentOffset { paragraph: 0, byte: 1 }), cx);
      editor.insert_text_command("Z", cx);
    });
    editor.read_with(cx, |editor, _| {
      assert_eq!(paragraph_text(editor.document(), 0), "XZY");
      assert_eq!(editor.selection().head, DocumentOffset { paragraph: 0, byte: 2 });
    });

    // B (concurrent) prepends "ABC" before the shared X.
    body_b.insert(1, "ABC").expect("B inserts");
    doc_b.commit();
    let update_b = doc_b
      .export(loro::ExportMode::updates(&base_vv))
      .expect("B delta");

    let forks_before = caret_rebase_fork_count();
    let mut guard = gate_a.lock(GateHolder::ImportChunk).expect("gate healthy");
    guard.import_remote_update(&update_b).expect("A imports B");
    drop(guard);
    editor.update(cx, |editor, cx| editor.sync_projection_from_authority(cx));
    let forks_after = caret_rebase_fork_count();

    editor.read_with(cx, |editor, _| {
      assert_eq!(paragraph_text(editor.document(), 0), "ABCXZY", "B's prepend merges before A's local edit");
      // Caret was after Z (byte 2 in "XZY"); after "ABC" prepended it is byte 5 in
      // "ABCXZY" ("ABCXZ|Y").
      assert_eq!(
        editor.selection().head,
        DocumentOffset { paragraph: 0, byte: 5 },
        "armed caret repositions across the remote insert"
      );
    });
    assert_eq!(forks_after - forks_before, 0, "an armed caret must use the fast cursor path, not fork_at");
  }

  /// FAST-path guard after a CARET MOVE (no write): moving the caret re-arms the
  /// anchor (via `emit_selection_changed`), so a subsequent remote insert-before-
  /// caret RESOLVES the stored cursors instead of the O(doc) `fork_at`. Regression
  /// guard for the freeze that shipped when re-arm happened only after a
  /// write/sync: the first edit — and any incoming remote edit — after every
  /// click/arrow fell to the ~350ms fork on a large doc.
  #[gpui::test]
  fn moved_caret_uses_fast_cursor_path_not_fork(cx: &mut gpui::TestAppContext) {
    use flowstate_collab::crdt_runtime::{CrdtRuntime, caret_rebase_fork_count};
    use flowstate_collab::local_write::GateHolder;

    let fixture = document_from_input(
      DocumentTheme::default(),
      vec![InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain("XYZW")],
      }],
    );
    let core_a = CrdtRuntime::from_document_projection(&fixture, "peer-A").expect("core A");
    let doc_b = core_a.doc().fork();
    doc_b.set_peer_id(0x00B0_B0B0).expect("distinct peer id");
    let base_vv = doc_b.state_vv();
    let body_b = flowstate_document::loro_schema::body_text(&doc_b);

    let (handle_a, gate_a) = LocalDocHandle::new(core_a, LocalWriteConfig::default());
    let projection_a = handle_a.projection().expect("A projection");
    let editor = cx.new(|cx| RichTextEditor::new_with_path(projection_a.clone(), None, cx));
    editor.update(cx, |editor, cx| {
      editor.set_write_authority(std::sync::Arc::new(handle_a), projection_a, cx);
    });

    // A MOVES the caret (no edit) to byte 2 ("XY|ZW"). The re-arm on selection
    // change captures the fast-path anchor for this position — the crux of the fix.
    editor.update(cx, |editor, cx| {
      editor.set_selection(EditorSelection::collapsed(DocumentOffset { paragraph: 0, byte: 2 }), cx);
    });

    // B (concurrent) prepends "ABC" before the shared text.
    body_b.insert(1, "ABC").expect("B inserts");
    doc_b.commit();
    let update_b = doc_b
      .export(loro::ExportMode::updates(&base_vv))
      .expect("B delta");

    let forks_before = caret_rebase_fork_count();
    let mut guard = gate_a.lock(GateHolder::ImportChunk).expect("gate healthy");
    guard.import_remote_update(&update_b).expect("A imports B");
    drop(guard);
    editor.update(cx, |editor, cx| editor.sync_projection_from_authority(cx));
    let forks_after = caret_rebase_fork_count();

    editor.read_with(cx, |editor, _| {
      assert_eq!(
        paragraph_text(editor.document(), 0),
        "ABCXYZW",
        "B's prepend merges before the shared text"
      );
      // Caret at byte 2 ("XY|ZW") shifts past the 3-char prepend to byte 5 ("ABCXY|ZW").
      assert_eq!(
        editor.selection().head,
        DocumentOffset { paragraph: 0, byte: 5 },
        "moved caret repositions across the remote insert"
      );
    });
    assert_eq!(
      forks_after - forks_before,
      0,
      "a caret moved (not written) must still use the fast cursor path, not fork_at"
    );
  }

  /// O-S5: whole-section move through the REAL authority — text + styles
  /// travel together, the move is one grouped undo, and the canonical doc
  /// (what peers converge on) agrees with the editor after move and undo.
  #[gpui::test]
  fn whole_section_move_is_grouped_undoable_and_canonical(cx: &mut gpui::TestAppContext) {
    let document = document_from_input(
      DocumentTheme::default(),
      vec![
        InputParagraph {
          style: ParagraphStyle::Custom(1),
          runs: vec![plain("Heading A")],
        },
        InputParagraph {
          style: ParagraphStyle::Normal,
          runs: vec![plain("body under A")],
        },
        InputParagraph {
          style: ParagraphStyle::Custom(1),
          runs: vec![plain("Heading B")],
        },
        InputParagraph {
          style: ParagraphStyle::Normal,
          runs: vec![plain("body under B")],
        },
      ],
    );
    let editor = editor_with_authority(document, cx);
    let texts = |editor: &gpui::Entity<RichTextEditor>, cx: &mut gpui::TestAppContext| -> Vec<String> {
      editor.read_with(cx, |editor, _| {
        (0..editor.document().paragraphs.len())
          .map(|ix| flowstate_document::paragraph_text(editor.document(), ix).to_string())
          .collect()
      })
    };
    assert_eq!(texts(&editor, cx), ["Heading A", "body under A", "Heading B", "body under B"]);

    // Move section A (paragraphs 0..2) below section B (target = end, ix 4).
    editor.update(cx, |editor, cx| {
      assert!(editor.move_paragraph_range(0, 2, 4, cx), "the move applies");
    });
    assert_eq!(
      texts(&editor, cx),
      ["Heading B", "body under B", "Heading A", "body under A"],
      "the whole section (heading + body) moved"
    );
    // Styles traveled with the text.
    editor.read_with(cx, |editor, _| {
      assert_eq!(editor.document().paragraphs[2].style, ParagraphStyle::Custom(1));
    });

    // ONE undo returns the original order (the move is one grouped intent).
    editor.update(cx, |editor, cx| editor.undo(cx));
    assert_eq!(
      texts(&editor, cx),
      ["Heading A", "body under A", "Heading B", "body under B"],
      "one undo restores the pre-move order"
    );

    // Refusals: overlapping target and out-of-range are rejected.
    editor.update(cx, |editor, cx| {
      assert!(!editor.move_paragraph_range(0, 2, 1, cx), "target inside the section refuses");
      assert!(!editor.move_paragraph_range(0, 9, 3, cx), "out-of-range refuses");
    });
  }
}
