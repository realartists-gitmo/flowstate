// The editor's Loro-first write path (spec §5, D2/D3).
//
// Every local mutation flows through here: build a typed intent from the
// user's action + selection, call the injected [`LocalWriteAuthority`]
// synchronously (the commit happens inside the call, under the write gate),
// and advance THE projection with the returned exact patches. There is no
// optimistic state, no command capture, no pending queue, no reconcile — the
// "optimistic" apply IS the committed apply.
//
// Rejections (spec I-15): restore the caret to the nearest valid position,
// surface a recovery notice, never retry unchanged, never synthesize a
// replacement mutation from the visible projection.

// NOTE: spliced into editor/mod.rs via include! like every editor submodule —
// names resolve in that scope; local-intents types are imported there.

impl RichTextEditor {
  /// Install the document's write authority (the ONE local write path —
  /// identical object for solo and collaborative documents, invariant 5) and
  /// adopt its canonical projection as THE document.
  pub fn set_write_authority(&mut self, authority: std::sync::Arc<dyn LocalWriteAuthority>, projection: DocumentProjection, cx: &mut Context<Self>) {
    self.write_authority = Some(authority);
    self.install_canonical_projection(projection, cx);
  }

  #[must_use]
  pub fn has_write_authority(&self) -> bool {
    self.write_authority.is_some()
  }

  /// Replace THE projection wholesale (authority attach, undo/redo, rare
  /// audited full rebuilds, remote `ProjectionUpdated`).
  pub fn install_canonical_projection(&mut self, projection: DocumentProjection, cx: &mut Context<Self>) {
    let theme = self.document.theme.clone();
    self.document = projection_with_local_theme(projection, &theme);
    self.identity_map.reconcile(&self.document);
    self.clamp_selection_to_document(cx);
    let generation = self.next_edit_generation;
    self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
    self.mark_document_changed(generation, false, cx);
  }

  /// Build a stable text anchor for a render-space offset (identity + hint;
  /// raw offsets never leave the editor as authority — spec I-2).
  #[must_use]
  pub(super) fn text_anchor_at(&self, offset: DocumentOffset) -> Option<TextAnchor> {
    let paragraph = self.identity_map.paragraph_id(offset.paragraph)?;
    Some(TextAnchor::new(paragraph, offset.byte))
  }

  /// Central dispatch: run one intent through the authority and integrate the
  /// outcome. Returns the commit on success. All `write_*` helpers route here.
  pub(super) fn write_intent(&mut self, intent: LocalIntent, cx: &mut Context<Self>) -> Option<LocalCommit> {
    let Some(authority) = self.write_authority.clone() else {
      // No authority ⇒ the document is read-only (display surface). Nothing
      // mutates locally; there is no fallback editing mode (invariant 5).
      return None;
    };
    let class = intent.class();
    match authority.apply(intent) {
      Ok(outcome) => Some(self.integrate_outcome(outcome, cx)),
      Err(rejection) => {
        self.handle_write_rejection(class, &rejection, cx);
        None
      },
    }
  }

  fn integrate_outcome(&mut self, outcome: LocalWriteOutcome, cx: &mut Context<Self>) -> LocalCommit {
    // Field fix 2026-07-07: the projection advances ONLY by draining the
    // core's ordered stream — the intent's own batch arrives as the tail of
    // the drain, AFTER any remote batches that committed before it. Applying
    // the returned batch directly (the old shape) raced async remote delivery
    // and produced the base-frontier mismatch cascade in the field logs.
    self.sync_projection_from_authority(cx);
    let commit = match outcome {
      LocalWriteOutcome::Committed(commit) => commit,
      LocalWriteOutcome::CommittedWithRebuild { commit, .. } => commit,
    };
    if let Some(selection) = &commit.selection_after {
      let caret = self.clamp_offset_to_document(selection.head.offset);
      self.set_caret_after_local_write(caret, cx);
    }
    commit
  }

  /// Drain the core's ordered projection stream and apply every item in
  /// commit order. The ONE way this editor's projection advances (local
  /// intents call it synchronously; the collaboration session calls it as a
  /// pump when remote batches land; solo documents need no pump).
  pub fn sync_projection_from_authority(&mut self, cx: &mut Context<Self>) {
    let Some(authority) = self.write_authority.clone() else {
      return;
    };
    let items = match authority.drain_projection_stream() {
      Ok(items) => items,
      Err(rejection) => {
        self.handle_write_rejection("projection-sync", &rejection, cx);
        return;
      },
    };
    if items.is_empty() {
      return;
    }
    let mut needs_self_heal = false;
    for item in items {
      match item {
        ProjectionStreamItem::Patches(batch) => {
          if batch.new_frontier == self.document.frontier {
            continue; // already applied (idempotent redelivery)
          }
          if batch.base_frontier != self.document.frontier {
            // Exactly-once ordered delivery makes this unreachable unless a
            // bookkeeping bug slipped in — heal from canonical, loudly.
            flowstate_fidelity::event(flowstate_fidelity::FidelityClass::Frontier, "stream-order-heal", || {
              format!("txn={} base_len={} doc_len={}", batch.transaction_id, batch.base_frontier.len(), self.document.frontier.len())
            });
            needs_self_heal = true;
            continue;
          }
          let mut document = self.document.clone();
          if apply_projection_patch_batch(&mut document, &batch).is_err() {
            needs_self_heal = true;
            continue;
          }
          let theme = self.document.theme.clone();
          self.document = projection_with_local_theme(document, &theme);
        },
        ProjectionStreamItem::Replace(document) => {
          let mut document = *document;
          // Asset BYTES are UI-cached sideband state; a canonical replace must
          // not drop bytes the cache already holds (the canonical store may
          // only carry metadata for assets this replica hasn't pulled).
          for (id, record) in &self.document.assets.assets {
            document.assets.assets.entry(*id).or_insert_with(|| record.clone());
          }
          let theme = self.document.theme.clone();
          self.document = projection_with_local_theme(document, &theme);
        },
      }
    }
    if needs_self_heal
      && let Ok(document) = authority.canonical_projection()
    {
      tracing::error!("projection stream self-heal: installing canonical projection");
      let theme = self.document.theme.clone();
      self.document = projection_with_local_theme(document, &theme);
    }
    self.identity_map.reconcile(&self.document);
    let head = self.clamp_offset_to_document(self.selection.head);
    let anchor = self.clamp_offset_to_document(self.selection.anchor);
    if head != self.selection.head || anchor != self.selection.anchor {
      self.selection = EditorSelection::range(anchor, head);
      self.emit_selection_changed(cx);
    }
    let generation = self.next_edit_generation;
    self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
    self.mark_document_changed(generation, false, cx);
  }

  /// Spec I-15 rejection handling: nearest-valid caret + loud notice; never an
  /// automatic retry.
  fn handle_write_rejection(&mut self, class: &'static str, rejection: &WriteRejected, cx: &mut Context<Self>) {
    tracing::warn!(class, %rejection, "local write intent rejected");
    flowstate_fidelity::event(flowstate_fidelity::FidelityClass::Structure, "write-intent-rejected", || {
      format!("class={class} rejection={rejection}")
    });
    match rejection {
      WriteRejected::EmptyIntent => {},
      WriteRejected::CompensatedFailure { .. } | WriteRejected::CompensationFailed { .. } | WriteRejected::GatePoisoned => {
        self.note_write_recovery(class, cx);
      },
      _ => {
        self.clamp_selection_to_document(cx);
        self.note_write_recovery(class, cx);
      },
    }
  }

  fn note_write_recovery(&mut self, class: &'static str, cx: &mut Context<Self>) {
    self.reconciliation_recoveries = self.reconciliation_recoveries.saturating_add(1);
    let _ = class;
    cx.notify();
  }

  pub(super) fn clamp_offset_to_document(&self, offset: DocumentOffset) -> DocumentOffset {
    let paragraph = offset.paragraph.min(self.document.paragraphs.len().saturating_sub(1));
    let byte = self
      .document
      .paragraphs
      .get(paragraph)
      .map(paragraph_text_len)
      .unwrap_or_default()
      .min(offset.byte);
    DocumentOffset { paragraph, byte }
  }

  fn clamp_selection_to_document(&mut self, cx: &mut Context<Self>) {
    let head = self.clamp_offset_to_document(self.selection.head);
    let anchor = self.clamp_offset_to_document(self.selection.anchor);
    self.selection = EditorSelection::range(anchor, head);
    self.emit_selection_changed(cx);
  }

  fn set_caret_after_local_write(&mut self, caret: DocumentOffset, cx: &mut Context<Self>) {
    self.selection = EditorSelection::collapsed(caret);
    self.pending_scroll_head_after_layout = true;
    self.emit_selection_changed(cx);
  }

  // ---- Command-level write helpers -----------------------------------------
  // These are the surface the command/action layer calls. Each builds an
  // intent from selection state, delegating selection-replacement rules here
  // so every entry point (keystroke, IME, paste, menus) shares one law.

  /// Insert plain text at the caret, replacing the active selection first.
  /// One user edit = one undo group: a selection replacement groups its
  /// delete+insert.
  pub(super) fn write_insert_text_at_caret(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
    if text.is_empty() {
      return false;
    }
    let grouped = !self.selection.is_caret();
    if grouped {
      self.begin_undo_group();
      if !self.write_delete_selection(cx) {
        self.end_undo_group();
        return false;
      }
    }
    let caret = self.selection.head;
    let Some(at) = self.text_anchor_at(caret) else {
      if grouped {
        self.end_undo_group();
      }
      return false;
    };
    let style_override = self.pending_styles.take();
    let committed = self
      .write_intent(
        LocalIntent::InsertText(InsertTextIntent {
          at,
          text: text.to_string(),
          style_override,
        }),
        cx,
      )
      .is_some();
    if grouped {
      self.end_undo_group();
    }
    committed
  }

  /// Delete the active selection (no-op on a bare caret).
  pub(super) fn write_delete_selection(&mut self, cx: &mut Context<Self>) -> bool {
    if self.selection.is_caret() {
      return false;
    }
    let range = self.selection.normalized();
    self.write_delete_offset_range(range, cx)
  }

  pub(super) fn write_delete_offset_range(&mut self, range: std::ops::Range<DocumentOffset>, cx: &mut Context<Self>) -> bool {
    let (Some(start), Some(end)) = (self.text_anchor_at(range.start), self.text_anchor_at(range.end)) else {
      return false;
    };
    self
      .write_intent(LocalIntent::DeleteRange(DeleteRangeIntent { start, end }), cx)
      .is_some()
  }

  /// Split the paragraph at the caret (Enter), replacing any selection first.
  pub(super) fn write_split_at_caret(&mut self, inherited_style: ParagraphStyle, cx: &mut Context<Self>) -> bool {
    let grouped = !self.selection.is_caret();
    if grouped {
      self.begin_undo_group();
      if !self.write_delete_selection(cx) {
        self.end_undo_group();
        return false;
      }
    }
    let caret = self.selection.head;
    let Some(at) = self.text_anchor_at(caret) else {
      if grouped {
        self.end_undo_group();
      }
      return false;
    };
    let committed = self
      .write_intent(LocalIntent::SplitParagraph(SplitParagraphIntent { at, inherited_style }), cx)
      .is_some();
    if grouped {
      self.end_undo_group();
    }
    committed
  }

  /// Join the caret's paragraph with its predecessor (Backspace at paragraph
  /// start) or successor (Delete at paragraph end).
  pub(super) fn write_join_paragraphs(&mut self, first_ix: usize, cx: &mut Context<Self>) -> bool {
    let (Some(first), Some(second)) = (self.identity_map.paragraph_id(first_ix), self.identity_map.paragraph_id(first_ix + 1)) else {
      return false;
    };
    // Caret lands at the junction point.
    let junction = DocumentOffset {
      paragraph: first_ix,
      byte: self.document.paragraphs.get(first_ix).map(paragraph_text_len).unwrap_or(0),
    };
    let committed = self
      .write_intent(LocalIntent::JoinParagraphs(JoinParagraphsIntent { first, second }), cx)
      .is_some();
    if committed {
      let caret = self.clamp_offset_to_document(junction);
      self.set_caret_after_local_write(caret, cx);
    }
    committed
  }

  /// Apply run styles over an offset range (minimal marks over the changed
  /// range only — spec §9).
  pub(super) fn write_set_marks(&mut self, range: std::ops::Range<DocumentOffset>, styles: RunStyles, cx: &mut Context<Self>) -> bool {
    let (Some(start), Some(end)) = (self.text_anchor_at(range.start), self.text_anchor_at(range.end)) else {
      return false;
    };
    self
      .write_intent(LocalIntent::SetMarks(SetMarksIntent { start, end, styles }), cx)
      .is_some()
  }

  pub(super) fn write_set_paragraph_style(&mut self, paragraph_ix: usize, style: ParagraphStyle, cx: &mut Context<Self>) -> bool {
    let Some(paragraph) = self.identity_map.paragraph_id(paragraph_ix) else {
      return false;
    };
    self
      .write_intent(LocalIntent::SetParagraphStyle(SetParagraphStyleIntent { paragraph, style }), cx)
      .is_some()
  }

  pub(super) fn write_insert_object_at_caret(&mut self, block: InputBlock, cx: &mut Context<Self>) -> bool {
    let caret = self.selection.head;
    let Some(at) = self.text_anchor_at(caret) else {
      return false;
    };
    self
      .write_intent(LocalIntent::InsertObject(InsertObjectIntent { at, block }), cx)
      .is_some()
  }

  pub(super) fn write_insert_rich_fragment_at_caret(&mut self, blocks: Vec<FragmentBlock>, cx: &mut Context<Self>) -> bool {
    let grouped = !self.selection.is_caret();
    if grouped {
      self.begin_undo_group();
      if !self.write_delete_selection(cx) {
        self.end_undo_group();
        return false;
      }
    }
    let caret = self.selection.head;
    let Some(at) = self.text_anchor_at(caret) else {
      if grouped {
        self.end_undo_group();
      }
      return false;
    };
    let committed = self
      .write_intent(LocalIntent::InsertRichFragment(InsertRichFragmentIntent { at, blocks }), cx)
      .is_some();
    if grouped {
      self.end_undo_group();
    }
    committed
  }

  // ---- Undo grouping (spec §10) ---------------------------------------------

  /// Open an undo group at an input-semantic boundary. Fallible by design
  /// (remote imports close groups); the editor simply re-arms next time.
  pub(super) fn begin_undo_group(&mut self) {
    if let Some(authority) = &self.write_authority
      && !matches!(authority.undo_group_start(), Ok(true))
    {
      tracing::debug!("undo group did not open (closed by import or already open); re-arming at next boundary");
    }
  }

  pub(super) fn end_undo_group(&mut self) {
    if let Some(authority) = &self.write_authority {
      let _ = authority.undo_group_end();
    }
  }

  // ---- Undo / redo (spec §10) -----------------------------------------------

  pub(super) fn undo_via_authority(&mut self, cx: &mut Context<Self>) -> bool {
    let Some(authority) = self.write_authority.clone() else {
      return false;
    };
    match authority.undo() {
      Ok(outcome) => self.apply_undo_outcome(outcome, cx),
      Err(rejection) => {
        self.handle_write_rejection("undo", &rejection, cx);
        false
      },
    }
  }

  pub(super) fn redo_via_authority(&mut self, cx: &mut Context<Self>) -> bool {
    let Some(authority) = self.write_authority.clone() else {
      return false;
    };
    match authority.redo() {
      Ok(outcome) => self.apply_undo_outcome(outcome, cx),
      Err(rejection) => {
        self.handle_write_rejection("redo", &rejection, cx);
        false
      },
    }
  }

  fn apply_undo_outcome(&mut self, outcome: LocalUndoOutcome, cx: &mut Context<Self>) -> bool {
    if outcome.replace.is_none() {
      return false; // empty undo/redo stack
    }
    // Field fix: the undo's projection change rides the ordered stream like
    // every other change; the outcome's payload is only the applied signal.
    self.sync_projection_from_authority(cx);
    if let Some(selection) = outcome.selection {
      let head = self.clamp_offset_to_document(selection.head);
      let anchor = self.clamp_offset_to_document(selection.anchor);
      self.selection = EditorSelection::range(anchor, head);
      self.emit_selection_changed(cx);
    } else {
      self.clamp_selection_to_document(cx);
    }
    true
  }
}
