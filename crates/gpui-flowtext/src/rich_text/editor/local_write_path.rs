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
    let mut incoming = projection_with_local_theme(projection, &theme);
    // §act-nine A9.3: canonical output restarts every paragraph version at 0;
    // carry surviving ids' versions forward so the content-keyed layout caches
    // can never serve a stale (style, version) collision.
    carry_forward_paragraph_versions(&self.document, &mut incoming);
    self.document = incoming;
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
    // Local variant: the caret is set from `commit.selection_after` below, so
    // the O(doc) caret rebase is skipped (it froze typing after every caret
    // move on large docs).
    self.sync_projection_local(cx);
    let commit = match outcome {
      LocalWriteOutcome::Committed(commit) => commit,
      LocalWriteOutcome::CommittedWithRebuild { commit, .. } => commit,
    };
    if let Some(selection) = &commit.selection_after {
      let caret = self.clamp_offset_to_document(selection.head.offset);
      self.set_caret_after_local_write(caret, cx);
    }
    // Re-arm the fast path for the NEW post-write caret (the capture inside the
    // sync above was for the pre-write selection). Keeps the typing peer on the
    // O(log n) path when a remote edit lands between its own keystrokes.
    self.capture_caret_anchor();
    commit
  }

  /// Drain the core's ordered projection stream and apply every item in
  /// commit order. The ONE way this editor's projection advances (local
  /// intents call it synchronously; the collaboration session calls it as a
  /// pump when remote batches land; solo documents need no pump).
  ///
  /// The remote pump passes `rebase_caret = true` so a remote insert/delete
  /// before the caret repositions it (the interleave fix). LOCAL callers pass
  /// `false`: they set the caret explicitly from their own commit right after
  /// this returns, so the caret-rebase here is not only wasted — its O(doc)
  /// `fork_at` fallback (taken whenever the caret moved since the last synced
  /// capture, i.e. the first edit after every click/arrow) froze typing on
  /// large docs for the whole fork (~350ms on a 2.3M-char doc).
  pub fn sync_projection_from_authority(&mut self, cx: &mut Context<Self>) {
    self.sync_projection_stream(cx, true);
  }

  /// Local-write variant: drain + apply patches WITHOUT the remote caret
  /// rebase (the caller overrides the caret from its own commit).
  pub(super) fn sync_projection_local(&mut self, cx: &mut Context<Self>) {
    self.sync_projection_stream(cx, false);
  }

  fn sync_projection_stream(&mut self, cx: &mut Context<Self>, rebase_caret: bool) {
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
    // §caret-anchor: reposition the caret across the remote edit. FAST path — when
    // the caret hasn't moved since its last synced capture, resolve the stored CRDT
    // cursors (O(log n)); SLOW fallback — reconstruct the pre-patch body and rebase
    // (O(doc) `fork_at`). Either way a remote insert/delete before the caret shifts
    // it instead of stranding it at a stale offset (the interleave bug). Captured
    // BEFORE the patches mutate `self.document`. `None` ⇒ clamp fallback.
    let reanchored_selection = rebase_caret
      .then(|| {
        self
          .caret_anchor
          .as_ref()
          .filter(|anchor| anchor.selection == self.selection)
          .and_then(|anchor| authority.resolve_selection_anchor(&anchor.head_cursor, &anchor.anchor_cursor))
          .map(|(head, anchor)| EditorSelection { anchor, head, ..self.selection.clone() })
          .or_else(|| authority.rebase_selection(&self.selection, &self.document, &self.document.frontier))
      })
      .flatten();
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
          // In-place apply: `apply_projection_patch_batch` is internally
          // transactional (content batches journal + roll back; structural
          // batches build a candidate), so the outer whole-projection clone
          // this used to make per batch was pure churn. Patches never touch
          // the local theme, so the former re-theme pass is dropped too. On
          // failure the canonical self-heal below replaces the document
          // wholesale anyway.
          if apply_projection_patch_batch(&mut self.document, &batch).is_err() {
            needs_self_heal = true;
            continue;
          }
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
          let mut incoming = projection_with_local_theme(document, &theme);
          // §act-nine A9.3: canonical replace = all versions 0; carry
          // surviving ids' versions forward (see install_canonical_projection).
          carry_forward_paragraph_versions(&self.document, &mut incoming);
          self.document = incoming;
        },
      }
    }
    if needs_self_heal
      && let Ok(document) = authority.canonical_projection()
    {
      tracing::error!("projection stream self-heal: installing canonical projection");
      let theme = self.document.theme.clone();
      let mut incoming = projection_with_local_theme(document, &theme);
      // §act-nine A9.3: same version carry-forward as every canonical install.
      carry_forward_paragraph_versions(&self.document, &mut incoming);
      self.document = incoming;
    }
    self.identity_map.reconcile(&self.document);
    // §caret-anchor: prefer the CRDT-reanchored selection (repositioned across the
    // remote edit); clamp only as a fallback when it couldn't be resolved. Both are
    // then clamped to the freshly-applied document as a final safety net.
    let next = reanchored_selection.unwrap_or_else(|| self.selection.clone());
    let head = self.clamp_offset_to_document(next.head);
    let anchor = self.clamp_offset_to_document(next.anchor);
    if head != self.selection.head || anchor != self.selection.anchor {
      self.selection = EditorSelection { anchor, head, ..next };
      self.emit_selection_changed(cx);
    }
    // Re-arm the fast path against the now-current core for the next remote patch.
    self.capture_caret_anchor();
    let generation = self.next_edit_generation;
    self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
    self.mark_document_changed(generation, false, cx);
  }

  /// §caret-anchor: snapshot the current selection's CRDT cursors while the editor
  /// and canonical core are in sync, so a subsequent remote patch can reposition
  /// the caret via [`crate::LocalWriteAuthority::resolve_selection_anchor`] without
  /// the O(doc) `fork_at` of the rebase fallback. Cheap no-op without an authority
  /// or when the caret can't be encoded (e.g. a non-body caret) — the fork rebase
  /// then covers it. Call at synced moments only (after a sync or a local write).
  pub(super) fn capture_caret_anchor(&mut self) {
    self.caret_anchor = self
      .write_authority
      .as_ref()
      .and_then(|authority| authority.encode_selection_anchor(&self.selection, &self.document.frontier))
      .map(|(head_cursor, anchor_cursor)| CaretAnchor {
        selection: self.selection.clone(),
        head_cursor,
        anchor_cursor,
      });
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

  /// Batched selection-wide restyle: ONE intent → one gate hold, one Loro
  /// commit, one undo member, one patch batch (§11 anti-amplification). The
  /// per-paragraph loop this replaces cost N full write-path round trips —
  /// 64.7s for a select-all restyle on the reference doc — and made undo
  /// replay N members.
  pub(super) fn write_set_paragraph_styles(&mut self, paragraph_ixs: impl IntoIterator<Item = usize>, style: ParagraphStyle, cx: &mut Context<Self>) -> bool {
    let paragraphs: Vec<_> = paragraph_ixs
      .into_iter()
      .filter_map(|paragraph_ix| self.identity_map.paragraph_id(paragraph_ix))
      .collect();
    if paragraphs.is_empty() {
      return false;
    }
    self
      .write_intent(LocalIntent::SetParagraphStyles(SetParagraphStylesIntent { paragraphs, style }), cx)
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

  /// Replace-all (find & replace): every same-paragraph match rides ONE
  /// compound intent — one gate hold, one commit, one undo member.
  pub(super) fn write_replace_matches(&mut self, matches: Vec<ReplaceMatch>, replacement: &str, cx: &mut Context<Self>) -> bool {
    if matches.is_empty() {
      return false;
    }
    self
      .write_intent(
        LocalIntent::ReplaceMatches(ReplaceMatchesIntent {
          matches,
          replacement: replacement.to_string(),
        }),
        cx,
      )
      .is_some()
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
    if !outcome.applied {
      return false; // empty undo/redo stack
    }
    // Field fix: the undo's projection change rides the ordered stream like
    // every other change; the outcome's payload is only the applied signal.
    // Local variant: the caret is set from `outcome.selection` below.
    self.sync_projection_local(cx);
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
