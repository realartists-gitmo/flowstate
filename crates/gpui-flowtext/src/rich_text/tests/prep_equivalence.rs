// NOTE: include!()-spliced into tests/mod.rs — imports live there.
//
// §act-nine A9.3 NET — content-keyed layout caches. The prep / shaping /
// chunk / height caches key on paragraph CONTENT (style, version) + stable
// identity, with no global edit generation: an edit to one paragraph must
// keep every other paragraph's cached layout, and a changed paragraph must
// NEVER be served stale. These tests are the oracle for that law:
//
// - cache-served prep must equal a freshly computed prep (built via the same
//   `build_paragraph_prep_from_parts` the pipeline uses) for every untouched
//   paragraph after an unrelated edit — if a future change misses a prep
//   input, fresh and cached diverge and this fails;
// - the edited paragraph's stale prep/chunks/height must not be served;
// - version RESETS (structural rebuilds, canonical installs) must not recreate
//   an old (style, version) pair for different content — the version-
//   discipline prerequisite, tested through the public install/patch paths;
// - a theme update must invalidate invisibility-mode prep (the theme feeds
//   `run_is_visible_for_theme`, which is not part of the prep key);
// - a positional slot reused by a DIFFERENT paragraph that shares
//   (style, version) must be a miss, not a stale hit (identity check).

fn a93_varied_document() -> DocumentProjection {
  let cite = RunStyles::default().with(RunStyle::Semantic(1));
  let spoken = RunStyles::default().with(RunStyle::Highlight(1));
  let mut theme = DocumentTheme::default();
  theme.set_invisibility_visible_semantic_style(1);
  theme.set_invisibility_visible_highlight_style(1);
  document_from_input(theme, vec![
    InputParagraph {
      style: ParagraphStyle::Custom(2),
      runs: vec![plain("A heading-styled paragraph")],
    },
    InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("Plain body text that will be edited in place by the test")],
    },
    InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("hidden lead-in "), InputRun { text: "cited evidence".into(), styles: cite }, InputRun {
        text: " spoken tail".into(),
        styles: spoken,
      }],
    },
    InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("混合 multibyte 内容 with soft\u{2028}break and wrap candidates")],
    },
    InputParagraph {
      style: ParagraphStyle::Normal,
      runs: Vec::new(),
    },
  ])
}

/// In-place single-paragraph content edit, shaped exactly like the write
/// path's `replace_paragraph_content`: rope splice + run replacement + a
/// version bump + offset/block mirror refresh. Followed by an editor
/// generation bump to prove cache validity does not depend on it.
fn a93_edit_paragraph_in_place(editor: &mut RichTextEditor, paragraph_ix: usize, new_text: &str, cx: &mut gpui::Context<RichTextEditor>) {
  let old = editor.document.paragraphs.get(paragraph_ix).expect("edit target").clone();
  let byte_range = crate::edit_ops::paragraph_byte_range(&editor.document, paragraph_ix);
  editor.document.text.delete(byte_range.clone());
  editor.document.text.insert(byte_range.start, new_text);
  paragraphs_mut(&mut editor.document).set(paragraph_ix, Paragraph {
    style: old.style,
    runs: vec![TextRun {
      len: new_text.len(),
      styles: RunStyles::default(),
    }],
    version: old.version.wrapping_add(1),
  });
  update_paragraph_offsets_after_len_change(&mut editor.document, paragraph_ix);
  // The old keys bundled `edit_generation`; advancing it here asserts the
  // content-keyed caches no longer care.
  editor.mark_as_unsaved_branch(cx);
}

fn a93_fresh_prep(editor: &RichTextEditor, paragraph_ix: usize) -> ParagraphPrep {
  let paragraph = editor.document.paragraphs.get(paragraph_ix).expect("paragraph").clone();
  let paragraph_id = editor.document.ids.paragraph_ids[paragraph_ix];
  let byte_range = crate::edit_ops::paragraph_byte_range(&editor.document, paragraph_ix);
  build_paragraph_prep_from_parts(
    &editor.document.text,
    &editor.document.theme,
    paragraph_id,
    paragraph_ix,
    &paragraph,
    byte_range,
    editor.invisibility_mode(),
  )
  .expect("fresh prep")
}

fn a93_assert_prep_eq(served: &ParagraphPrep, fresh: &ParagraphPrep, paragraph_ix: usize) {
  assert_eq!(served.key, fresh.key, "prep key mismatch at paragraph {paragraph_ix}");
  assert_eq!(served.paragraph_id, fresh.paragraph_id, "prep id mismatch at paragraph {paragraph_ix}");
  assert_eq!(served.paragraph_ix, fresh.paragraph_ix, "prep ix mismatch at paragraph {paragraph_ix}");
  assert_eq!(served.paragraph_text, fresh.paragraph_text, "prep text mismatch at paragraph {paragraph_ix}");
  assert_eq!(
    served.layout_runs.as_ref(),
    fresh.layout_runs.as_ref(),
    "prep runs mismatch at paragraph {paragraph_ix}"
  );
  assert_eq!(served.layout_style, fresh.layout_style, "prep style mismatch at paragraph {paragraph_ix}");
  assert_eq!(served.layout_version, fresh.layout_version, "prep version mismatch at paragraph {paragraph_ix}");
  assert_eq!(served.source_len, fresh.source_len, "prep source_len mismatch at paragraph {paragraph_ix}");
  assert_eq!(
    served.wrap_break_ends.as_ref(),
    fresh.wrap_break_ends.as_ref(),
    "prep wrap breaks mismatch at paragraph {paragraph_ix}"
  );
  assert_eq!(served.visible, fresh.visible, "prep visibility mismatch at paragraph {paragraph_ix}");
}

/// §act-eleven C4 tripwire: an unrelated single-paragraph edit must rebuild a
/// BOUNDED number of preps on the next full relayout — not the whole viewport.
/// The equality tests prove correctness; this proves EFFECTIVENESS (the T8.12
/// "landed but inert" failure class trips this immediately).
#[gpui::test]
fn single_edit_rebuilds_bounded_prep_count(cx: &mut gpui::TestAppContext) {
  cx.update(gpui_component::init);
  let handle = cx.add_window(|_window, cx| RichTextEditor::new_with_path(a93_varied_document(), None, cx));
  handle
    .update(cx, |editor, window, cx| {
      let width = px(760.0);
      let count = editor.document.paragraphs.len();
      for paragraph_ix in 0..count {
        let _ = editor.layout_paragraph_chunk_for_element(paragraph_ix, 0, width, window, cx);
      }
      // Warm: every paragraph has prep. Now one in-place edit...
      crate::rich_text::layout::reset_prep_build_count();
      a93_edit_paragraph_in_place(editor, 1, "tripwire body — bounded rebuilds only", cx);
      for paragraph_ix in 0..count {
        let _ = editor.layout_paragraph_chunk_for_element(paragraph_ix, 0, width, window, cx);
      }
      let rebuilds = crate::rich_text::layout::prep_build_count();
      // The edited paragraph rebuilds (possibly once per visibility variant);
      // everything else must be served from the content-keyed cache. 4 is a
      // generous bound for ONE edited paragraph; whole-viewport regression on
      // this fixture would be >= `count` (all paragraphs).
      assert!(
        rebuilds <= 4,
        "single in-place edit triggered {rebuilds} prep builds (bound 4) — the content-keyed cache has gone inert"
      );
      assert!(rebuilds >= 1, "edited paragraph never re-prepped — vacuous tripwire");
    })
    .expect("window update");
}

/// Unrelated edits must keep every OTHER paragraph's prep, shaping key, chunk
/// layout, and exact height — and must never serve the edited paragraph stale.
#[gpui::test]
fn unrelated_edit_preserves_other_paragraphs_layout_caches(cx: &mut gpui::TestAppContext) {
  cx.update(gpui_component::init);
  let handle = cx.add_window(|_window, cx| RichTextEditor::new_with_path(a93_varied_document(), None, cx));
  handle
    .update(cx, |editor, window, cx| {
      let width = px(760.0);
      let count = editor.document.paragraphs.len();
      // Populate chunk + height + prep + shaping caches through the real
      // layout machinery.
      for paragraph_ix in 0..count {
        let _ = editor.layout_paragraph_chunk_for_element(paragraph_ix, 0, width, window, cx);
      }
      let mut before = Vec::new();
      for paragraph_ix in 0..count {
        let prep = editor.ensure_paragraph_prep_sync(paragraph_ix).expect("prep populated");
        let work_key = editor.paragraph_work_key(prep.as_ref(), width);
        let chunk_cached = editor.valid_chunk_cache_entry(paragraph_ix, width).is_some();
        let height = editor.valid_paragraph_height(paragraph_ix, width);
        before.push((prep, work_key, chunk_cached, height));
      }
      // Sanity: the visible short paragraphs completed, so heights are cached.
      assert!(before.iter().any(|(_, _, _, height)| height.is_some()), "height cache never populated — vacuous test");

      let edited = 1usize;
      a93_edit_paragraph_in_place(editor, edited, "a completely different body after the in-place edit", cx);

      for (paragraph_ix, entry) in before.iter().enumerate().take(count) {
        let (prep_before, work_key_before, chunk_before, height_before) = entry;
        if paragraph_ix == edited {
          assert!(
            editor.valid_paragraph_prep(paragraph_ix).is_none(),
            "edited paragraph served STALE prep"
          );
          assert!(
            editor.valid_chunk_cache_entry(paragraph_ix, width).is_none(),
            "edited paragraph served STALE chunk layout"
          );
          assert!(
            editor.valid_paragraph_height(paragraph_ix, width).is_none(),
            "edited paragraph served STALE height"
          );
          let fresh = editor.ensure_paragraph_prep_sync(paragraph_ix).expect("rebuilt prep");
          assert_eq!(fresh.paragraph_text.as_ref(), "a completely different body after the in-place edit");
          assert_ne!(
            editor.paragraph_work_key(fresh.as_ref(), width),
            *work_key_before,
            "edited paragraph's shaping work key must change"
          );
          continue;
        }
        let served = editor
          .valid_paragraph_prep(paragraph_ix)
          .unwrap_or_else(|| panic!("unchanged paragraph {paragraph_ix} lost its prep across an unrelated edit"));
        assert!(
          std::sync::Arc::ptr_eq(&served, prep_before),
          "unchanged paragraph {paragraph_ix} was re-prepped instead of cache-served"
        );
        // The cache-served prep must equal a prep computed FRESH from the
        // current document through the same pipeline function.
        let fresh = a93_fresh_prep(editor, paragraph_ix);
        a93_assert_prep_eq(&served, &fresh, paragraph_ix);
        // Shaping-cache validity decision: the work key (content key + width +
        // layout generation) is unchanged, so shaped fragments stay valid.
        assert_eq!(
          editor.paragraph_work_key(served.as_ref(), width),
          *work_key_before,
          "unchanged paragraph {paragraph_ix}'s shaping work key changed"
        );
        assert_eq!(
          editor.valid_chunk_cache_entry(paragraph_ix, width).is_some(),
          *chunk_before,
          "unchanged paragraph {paragraph_ix} lost its chunk layout"
        );
        assert_eq!(
          editor.valid_paragraph_height(paragraph_ix, width),
          *height_before,
          "unchanged paragraph {paragraph_ix} lost its cached height"
        );
      }
    })
    .expect("windowed layout pass");
}

/// The async install gate: a background prep batch completed AFTER an
/// unrelated edit must still install for the untouched paragraphs (content
/// still current) and must discard only the edited paragraph's stale prep.
#[gpui::test]
fn background_prep_batch_installs_for_still_current_content(cx: &mut gpui::TestAppContext) {
  cx.update(gpui_component::init);
  let handle = cx.add_window(|_window, cx| RichTextEditor::new_with_path(a93_varied_document(), None, cx));
  handle
    .update(cx, |editor, _window, cx| {
      let width = px(760.0);
      let count = editor.document.paragraphs.len();
      let batch = paragraph_prep_batch_request(&editor.document, false, (0..count).collect(), count.max(1), usize::MAX);
      let edited = 1usize;
      a93_edit_paragraph_in_place(editor, edited, "edited while the batch was in flight", cx);
      let result = build_paragraph_prep_batch(batch);
      editor.install_layout_prep_batch(width, result, cx);
      for paragraph_ix in 0..count {
        if paragraph_ix == edited {
          assert!(
            editor.valid_paragraph_prep(paragraph_ix).is_none(),
            "stale background prep installed for the edited paragraph"
          );
        } else {
          let served = editor
            .valid_paragraph_prep(paragraph_ix)
            .unwrap_or_else(|| panic!("background prep for still-current paragraph {paragraph_ix} was discarded"));
          a93_assert_prep_eq(&served, &a93_fresh_prep(editor, paragraph_ix), paragraph_ix);
        }
      }
    })
    .expect("windowed pass");
}

/// Version-discipline prerequisite (b): a canonical install replaces the
/// projection with all-version-0 materializer output. A surviving paragraph id
/// with DIFFERENT content must not collide with its old (style, version) cache
/// entries — versions carry forward, and no stale prep is ever served.
#[gpui::test]
fn canonical_install_with_reset_versions_never_serves_stale_prep(cx: &mut gpui::TestAppContext) {
  cx.update(gpui_component::init);
  let handle = cx.add_window(|_window, cx| RichTextEditor::new_with_path(a93_varied_document(), None, cx));
  handle
    .update(cx, |editor, _window, cx| {
      let count = editor.document.paragraphs.len();
      for paragraph_ix in 0..count {
        let _ = editor.ensure_paragraph_prep_sync(paragraph_ix);
      }
      let changed = 1usize;
      let old_text = editor
        .ensure_paragraph_prep_sync(changed)
        .expect("prep")
        .paragraph_text
        .clone();

      // Materializer-shaped replacement: same ids, same styles, all versions
      // RESET to 0, different content for one surviving paragraph.
      let cite = RunStyles::default().with(RunStyle::Semantic(1));
      let spoken = RunStyles::default().with(RunStyle::Highlight(1));
      let mut replacement = document_from_input(editor.document.theme.clone(), vec![
        InputParagraph {
          style: ParagraphStyle::Custom(2),
          runs: vec![plain("A heading-styled paragraph")],
        },
        InputParagraph {
          style: ParagraphStyle::Normal,
          runs: vec![plain("replaced content arriving at version zero")],
        },
        InputParagraph {
          style: ParagraphStyle::Normal,
          runs: vec![plain("hidden lead-in "), InputRun { text: "cited evidence".into(), styles: cite }, InputRun {
            text: " spoken tail".into(),
            styles: spoken,
          }],
        },
        InputParagraph {
          style: ParagraphStyle::Normal,
          runs: vec![plain("混合 multibyte 内容 with soft\u{2028}break and wrap candidates")],
        },
        InputParagraph {
          style: ParagraphStyle::Normal,
          runs: Vec::new(),
        },
      ]);
      assert!(replacement.paragraphs.iter().all(|paragraph| paragraph.version == 0));
      replacement.ids = editor.document.ids.clone();

      editor.install_canonical_projection(replacement, cx);

      // Surviving ids must have ADVANCED versions — never a reused 0.
      for paragraph_ix in 0..count {
        assert!(
          editor.document.paragraphs.get(paragraph_ix).expect("paragraph").version > 0,
          "canonical install reset a surviving paragraph's version to 0 (paragraph {paragraph_ix})"
        );
      }
      // And whatever the cache now serves must match the CURRENT content.
      if let Some(served) = editor.valid_paragraph_prep(changed) {
        assert_ne!(served.paragraph_text, old_text, "stale prep served after a canonical install");
      }
      let fresh = editor.ensure_paragraph_prep_sync(changed).expect("prep");
      assert_eq!(fresh.paragraph_text.as_ref(), "replaced content arriving at version zero");
    })
    .expect("windowed pass");
}

/// Version-discipline prerequisite (a): the structural-batch rebuild path
/// materializes surviving paragraphs through `paragraph_from_input_paragraph`
/// (version 0). Driving a real non-simple patch batch through the public apply
/// path must ADVANCE the surviving paragraph's version and drop its old prep.
#[gpui::test]
fn structural_rebuild_advances_surviving_paragraph_versions(cx: &mut gpui::TestAppContext) {
  cx.update(gpui_component::init);
  let handle = cx.add_window(|_window, cx| RichTextEditor::new_with_path(a93_varied_document(), None, cx));
  handle
    .update(cx, |editor, _window, cx| {
      let target = 2usize;
      for paragraph_ix in 0..editor.document.paragraphs.len() {
        let _ = editor.ensure_paragraph_prep_sync(paragraph_ix);
      }
      let old_version = editor.document.paragraphs.get(target).expect("target").version;
      let old_text = editor.ensure_paragraph_prep_sync(target).expect("prep").paragraph_text.clone();
      let target_paragraph_id = editor.document.ids.paragraph_ids[target];
      // Paragraph-only document: block row == paragraph index.
      let target_block_id = editor.document.ids.block_ids[target];
      // A leading structural insert forces the legacy full-rebuild path (the
      // in-place fast path only accepts content patches with one TRAILING
      // structural patch), so the trailing content patch flows through the
      // rebuilt-blocks arm — the exact `version: 0` hazard site.
      let batch = ProjectionPatchBatch {
        transaction_id: 1,
        base_frontier: editor.document.frontier.clone(),
        new_frontier: vec![1],
        patches: vec![
          ProjectionPatch::InsertBlocks {
            before: None,
            row_hint: editor.document.blocks.len(),
            blocks: vec![ProjectionStructuralBlock {
              block_id: new_block_id(),
              paragraph_id: Some(new_paragraph_id()),
              block: InputBlock::Paragraph(InputParagraph {
                style: ParagraphStyle::Normal,
                runs: vec![plain("appended tail paragraph")],
              }),
            }],
          },
          ProjectionPatch::ParagraphText {
            block_id: target_block_id,
            paragraph_id: target_paragraph_id,
            row_hint: target,
            new: InputParagraph {
              style: ParagraphStyle::Normal,
              runs: vec![plain("rebuilt with different content")],
            },
            delta_utf8: Vec::new(),
          },
        ]
        .into(),
      };
      editor.apply_remote_patch_batch(&batch, cx).expect("patch batch applies");

      let rebuilt = editor.document.paragraphs.get(target).expect("target").clone();
      assert!(
        rebuilt.version > old_version,
        "structural rebuild reset the surviving paragraph's version ({} -> {})",
        old_version,
        rebuilt.version
      );
      if let Some(served) = editor.valid_paragraph_prep(target) {
        assert_ne!(served.paragraph_text, old_text, "stale prep served after a structural rebuild");
      }
      let fresh = editor.ensure_paragraph_prep_sync(target).expect("prep");
      assert_eq!(fresh.paragraph_text.as_ref(), "rebuilt with different content");
    })
    .expect("windowed pass");
}

/// The theme feeds invisibility-mode prep (`run_is_visible_for_theme`) but is
/// not part of the prep key — a theme update must clear the prep map (the
/// pre-existing stale-prep hazard fixed with A9.3).
#[gpui::test]
fn theme_update_invalidates_invisibility_prep(cx: &mut gpui::TestAppContext) {
  cx.update(gpui_component::init);
  let cite = RunStyles::default().with(RunStyle::Semantic(1));
  let document = document_from_input(DocumentTheme::default(), vec![InputParagraph {
    style: ParagraphStyle::Normal,
    runs: vec![plain("hidden "), InputRun { text: "cited".into(), styles: cite }],
  }]);
  let handle = cx.add_window(|_window, cx| RichTextEditor::new_with_path(document, None, cx));
  handle
    .update(cx, |editor, _window, cx| {
      editor.set_invisibility_mode(true, cx);
      let stale = editor.ensure_paragraph_prep_sync(0).expect("prep");
      assert!(!stale.visible, "semantic slot 1 is not visible yet");
      // The theme change flips the same paragraph's projected visibility;
      // (style, version) is unchanged, so only a prep-map clear prevents a
      // stale hidden prep from being served.
      editor.update_document_theme(|theme| theme.set_invisibility_visible_semantic_style(1), cx);
      assert!(
        editor.valid_paragraph_prep(0).is_none(),
        "theme update left stale invisibility prep in the cache"
      );
      let fresh = editor.ensure_paragraph_prep_sync(0).expect("prep");
      assert!(fresh.visible, "rebuilt prep must reflect the new theme");
      assert_eq!(fresh.paragraph_text.as_ref(), "cited");
    })
    .expect("windowed pass");
}

/// A POSITIONAL cache slot (chunk/height) reused by a DIFFERENT paragraph
/// that happens to share (style, version) must be a MISS — the stable-id
/// check is what makes dropping the global edit generation safe. The
/// ID-KEYED prep cache has the opposite obligation (§act-eleven A11.7): a
/// prep must FOLLOW its paragraph identity across the swap.
#[gpui::test]
fn positional_caches_reject_slot_reuse_by_a_different_paragraph(cx: &mut gpui::TestAppContext) {
  cx.update(gpui_component::init);
  let document = document_from_input(DocumentTheme::default(), vec![
    InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("first paragraph body")],
    },
    InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("second paragraph carrying different words")],
    },
  ]);
  let handle = cx.add_window(|_window, cx| RichTextEditor::new_with_path(document, None, cx));
  handle
    .update(cx, |editor, window, cx| {
      let width = px(760.0);
      for paragraph_ix in 0..2 {
        let _ = editor.layout_paragraph_chunk_for_element(paragraph_ix, 0, width, window, cx);
        assert!(editor.valid_chunk_cache_entry(paragraph_ix, width).is_some());
      }
      // Both paragraphs share (Normal, version 0) — identical cache keys.
      // Swap them (paragraphs AND ids AND text), leaving the positional
      // chunk/height slots holding the OTHER paragraph's layout.
      let first = editor.document.paragraphs.get(0).expect("first").clone();
      let second = editor.document.paragraphs.get(1).expect("second").clone();
      assert_eq!(
        paragraph_cache_key(&editor.document, &first),
        paragraph_cache_key(&editor.document, &second),
        "fixture must collide on (style, version) for the test to bite"
      );
      editor.document.text = crop::Rope::from("second paragraph carrying different words\nfirst paragraph body");
      paragraphs_mut(&mut editor.document).set(0, second);
      paragraphs_mut(&mut editor.document).set(1, first);
      std::sync::Arc::make_mut(&mut editor.document.ids.paragraph_ids).swap(0, 1);
      std::sync::Arc::make_mut(&mut editor.document.ids.block_ids).swap(0, 1);
      update_paragraph_offsets_after_len_change(&mut editor.document, 0);
      update_paragraph_offsets_after_len_change(&mut editor.document, 1);

      for paragraph_ix in 0..2 {
        assert!(
          editor.valid_chunk_cache_entry(paragraph_ix, width).is_none(),
          "chunk slot {paragraph_ix} served a different paragraph's layout (identity check missing)"
        );
        assert!(
          editor.valid_paragraph_height(paragraph_ix, width).is_none(),
          "height slot {paragraph_ix} served a different paragraph's height (identity check missing)"
        );
        // §act-eleven A11.7: the prep cache is ID-KEYED and position-free —
        // ids moved WITH their content, so each paragraph's own prep must
        // FOLLOW it to the new index (this is the structural-shift win).
        let prep = editor
          .valid_paragraph_prep(paragraph_ix)
          .expect("id-keyed prep must survive an identity swap (it moved with the paragraph)");
        assert_eq!(
          Some(prep.paragraph_id),
          editor.paragraph_id_at(paragraph_ix),
          "prep at slot {paragraph_ix} belongs to a different paragraph identity"
        );
      }
    })
    .expect("windowed pass");
}

/// §act-eleven C9: hit-test geometry properties over the real layout — every
/// sampled pixel must resolve to a VALID document offset (in-bounds paragraph,
/// in-bounds byte, char boundary), paragraphs must be non-decreasing as y
/// grows at fixed x, and bytes non-decreasing as x grows within one paragraph
/// at fixed y. Paint/hit geometry previously had NO oracle at all.
#[gpui::test]
fn hit_test_resolves_valid_monotonic_offsets(cx: &mut gpui::TestAppContext) {
  cx.update(gpui_component::init);
  let handle = cx.add_window(|_window, cx| RichTextEditor::new_with_path(a93_varied_document(), None, cx));
  handle
    .update(cx, |editor, window, cx| {
      let width = px(760.0);
      let count = editor.document.paragraphs.len();
      for paragraph_ix in 0..count {
        let _ = editor.layout_paragraph_chunk_for_element(paragraph_ix, 0, width, window, cx);
      }
      let mut previous_paragraph_at_x: std::collections::HashMap<i32, usize> = std::collections::HashMap::new();
      for y_step in 0..40 {
        let y = px(y_step as f32 * 6.0);
        let mut previous: Option<crate::DocumentOffset> = None;
        for x_step in 0..20 {
          let x = px(x_step as f32 * 38.0);
          let offset = editor.hit_test_document_position(gpui::point(x, y), window, cx);
          // Validity.
          assert!(offset.paragraph < count, "hit at ({x_step},{y_step}) resolved out-of-bounds paragraph {}", offset.paragraph);
          let text = crate::paragraph_text(&editor.document, offset.paragraph);
          assert!(offset.byte <= text.len(), "hit at ({x_step},{y_step}) resolved out-of-bounds byte");
          assert!(text.is_char_boundary(offset.byte), "hit at ({x_step},{y_step}) resolved a non-char-boundary byte");
          // x-monotonicity within a paragraph at fixed y.
          if let Some(previous) = previous
            && previous.paragraph == offset.paragraph
          {
            assert!(
              offset.byte >= previous.byte,
              "byte went backwards left-to-right at y_step {y_step}: {} -> {}",
              previous.byte,
              offset.byte
            );
          }
          previous = Some(offset);
          // y-monotonicity at fixed x.
          let column = x_step;
          if let Some(&previous_paragraph) = previous_paragraph_at_x.get(&column) {
            assert!(
              offset.paragraph >= previous_paragraph,
              "paragraph went backwards top-to-bottom at x_step {column}: {previous_paragraph} -> {}",
              offset.paragraph
            );
          }
          previous_paragraph_at_x.insert(column, offset.paragraph);
        }
      }
    })
    .expect("window update");
}

/// §act-eleven A11.7: a STRUCTURAL shift (row inserted above) must keep every
/// shifted paragraph's prep — content keys don't move with position. Bound the
/// rebuilds with the C4 tripwire: only the NEW paragraph preps.
#[gpui::test]
fn structural_shift_keeps_shifted_tail_preps(cx: &mut gpui::TestAppContext) {
  cx.update(gpui_component::init);
  let handle = cx.add_window(|_window, cx| RichTextEditor::new_with_path(a93_varied_document(), None, cx));
  handle
    .update(cx, |editor, window, cx| {
      let width = px(760.0);
      let count = editor.document.paragraphs.len();
      for paragraph_ix in 0..count {
        let _ = editor.layout_paragraph_chunk_for_element(paragraph_ix, 0, width, window, cx);
      }
      let before: Vec<_> = (0..count)
        .map(|paragraph_ix| editor.valid_paragraph_prep(paragraph_ix).expect("prep populated"))
        .collect();

      // Insert a NEW paragraph row at the very top (the Enter-above shape),
      // mirroring the in-place splice the projection apply performs.
      crate::rich_text::layout::reset_prep_build_count();
      let new_paragraph = crate::Paragraph {
        style: crate::ParagraphStyle::Normal,
        runs: vec![crate::TextRun {
          len: "inserted-above".len(),
          styles: crate::RunStyles::default(),
        }],
        version: 7,
      };
      let document = &mut editor.document;
      document.text.insert(0, "inserted-above\n");
      crate::paragraphs_mut(document).insert(0, new_paragraph.clone());
      std::sync::Arc::make_mut(&mut document.ids.paragraph_ids).insert(0, crate::new_paragraph_id());
      document.blocks.insert(0, crate::Block::Paragraph(new_paragraph));
      std::sync::Arc::make_mut(&mut document.ids.block_ids).insert(0, crate::new_block_id());
      crate::rebuild_document_sections(document);

      let count = editor.document.paragraphs.len();
      for paragraph_ix in 0..count {
        let _ = editor.layout_paragraph_chunk_for_element(paragraph_ix, 0, width, window, cx);
      }
      // Every SHIFTED paragraph must be served its ORIGINAL prep (same Arc).
      for (old_ix, old_prep) in before.iter().enumerate() {
        let shifted_ix = old_ix + 1;
        let served = editor
          .valid_paragraph_prep(shifted_ix)
          .unwrap_or_else(|| panic!("shifted paragraph {shifted_ix} lost its prep"));
        assert!(
          std::sync::Arc::ptr_eq(&served, old_prep),
          "shifted paragraph {shifted_ix} re-prepped despite unchanged content (positional residual regressed)"
        );
      }
      let rebuilds = crate::rich_text::layout::prep_build_count();
      assert!(
        rebuilds <= 3,
        "structural shift triggered {rebuilds} prep builds (bound 3: the inserted paragraph only)"
      );
    })
    .expect("window update");
}
