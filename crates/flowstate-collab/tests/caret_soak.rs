// Headless soak test for the optimistic edit -> flush -> runtime -> reconcile
// loop, with the flowstate-fidelity caret/text/reconcile invariants armed.
//
// It drives a real gpui-flowtext editor through the exact sequence the app's
// flush_document_runtime_edits / apply_local_runtime_commit use: take the
// pending runtime edits, open the one-outstanding-transaction gate, apply the
// batch through a real CrdtRuntime (apply_editor_commands), feed the returned
// ProjectionPatched / ProjectionUpdated events back into the editor
// (apply_projection_patch_batch / replace_document_projection_replaying_pending),
// then complete_runtime_transaction. The one behavior it deliberately adds is
// the race that reproduces "typed text outruns the cursor": newer optimistic
// keystrokes are queued behind an in-flight transaction and only reconciled when
// that transaction completes.
//
// Two editors run the identical op stream. The "reference" flushes fully after
// every op (a purely-synchronous editor) and is the shadow expected-caret. The
// "subject" flushes on a schedule that keeps edits queued behind an in-flight
// transaction (the race). After every step the subject's caret and paragraph
// text must equal the reference's, and the armed fidelity invariants must stay
// silent. Either signal firing localizes a real reconciliation bug to the exact
// step (and dumps the ring-buffer trail via violation()), without this test
// needing to know the mechanism.

#[cfg(test)]
mod tests {
  use std::sync::Mutex;

  use flowstate_collab::crdt_runtime::{CrdtRuntime, EditorCommitResult, RuntimeEvent};
  use flowstate_fidelity as fidelity;
  use gpui::{AppContext as _, Entity, TestAppContext};
  use gpui_flowtext::{DocumentOffset, EditorSelection, RichTextEditor, SemanticCommandBatch, SemanticEditCommand, paragraph_text};

  // fidelity::enabled() is a process-global switch and both tests arm it, so
  // serialize them. The per-thread violation sink is otherwise isolated.
  static SERIAL: Mutex<()> = Mutex::new(());

  fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
  }

  // Small deterministic PRNG (SplitMix64) so a failure is reproducible.
  struct Prng {
    state: u64,
  }

  impl Prng {
    fn new(seed: u64) -> Self {
      Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
      self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
      let mut z = self.state;
      z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
      z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
      z ^ (z >> 31)
    }

    fn below(&mut self, bound: u64) -> u64 {
      self.next_u64() % bound
    }
  }

  // Editor operations the soak script mixes. Every variant has a caret effect
  // that is independent of visual layout (no soft-wrap or vertical motion), so a
  // second real editor driven synchronously is a faithful shadow.
  #[derive(Clone, Copy, Debug)]
  enum Op {
    Insert(char),
    Enter,
    Backspace,
    Delete,
    WordLeft,
    WordRight,
    DocStart,
    DocEnd,
    SelectAll,
    ToggleUnderline,
  }

  // The one-outstanding transaction the app keeps in flight while the async
  // runtime apply resolves. Held explicitly here so newer keystrokes can be
  // queued behind it before it is completed.
  struct InFlight {
    txn: u128,
    base: Vec<u8>,
    commands: Vec<SemanticEditCommand>,
    selection_after: Option<EditorSelection>,
  }

  // A real editor plus its own real runtime, and the harness reproducing the
  // app's flush protocol.
  struct Harness {
    entity: Entity<RichTextEditor>,
    runtime: CrdtRuntime,
    in_flight: Option<InFlight>,
    fallback_txn: u128,
  }

  impl Harness {
    fn new(cx: &mut TestAppContext, title: &str) -> Self {
      let runtime = CrdtRuntime::new_empty(title).expect("new_empty runtime");
      let seed = runtime.projection_snapshot().expect("initial projection snapshot");
      let entity = cx.update(|cx| cx.new(|cx| RichTextEditor::new_with_path(seed, None, cx)));
      // Route edits through the runtime capture path so take_pending_runtime_edits
      // returns the semantic command batches, exactly as the app configures a
      // collaborating document editor.
      cx.update(|cx| entity.update(cx, |editor, _| editor.set_runtime_capture(true)));
      Self {
        entity,
        runtime,
        in_flight: None,
        fallback_txn: 1,
      }
    }

    // Apply one editor operation optimistically. Mutations both update the
    // visible document/caret and append to the editor's pending runtime edits;
    // caret moves are layout-independent so they are deterministic without a
    // window.
    fn apply(&mut self, cx: &mut TestAppContext, op: Op) {
      cx.update(|cx| {
        self.entity.update(cx, |editor, cx| match op {
          Op::Insert(ch) => editor.insert_text_command(&ch.to_string(), cx),
          Op::Enter => editor.insert_paragraph_break_command(cx),
          Op::Backspace => editor.backspace_command(cx),
          Op::Delete => editor.delete_forward_command(cx),
          Op::WordLeft => editor.move_word_left(cx),
          Op::WordRight => editor.move_word_right(cx),
          Op::DocStart => editor.move_document_start(cx),
          Op::DocEnd => editor.move_document_end(cx),
          Op::SelectAll => editor.select_all(cx),
          Op::ToggleUnderline => editor.toggle_underline(cx),
        });
      });
    }

    // Open the one-outstanding transaction gate from whatever edits are pending,
    // mirroring flush_document_runtime_edits up to the point the async runtime
    // apply is spawned. Returns whether a transaction was actually begun.
    fn begin(&mut self, cx: &mut TestAppContext) -> bool {
      if self.in_flight.is_some() {
        return false;
      }
      let edits = cx.update(|cx| self.entity.update(cx, |editor, _| editor.take_pending_runtime_edits()));
      if edits.is_empty() {
        return false;
      }
      let (txn, base, commands, selection_after) = flatten(edits, &mut self.fallback_txn);
      if commands.is_empty() {
        // No document-changing work; nothing to flush.
        return false;
      }
      cx.update(|cx| self.entity.update(cx, |editor, _| editor.begin_runtime_transaction(txn)));
      self.in_flight = Some(InFlight {
        txn,
        base,
        commands,
        selection_after,
      });
      true
    }

    // Resolve the in-flight transaction against the runtime and feed the commit
    // back into the editor exactly as apply_local_runtime_commit does. Any
    // keystrokes applied since begin are still queued as pending and get replayed
    // onto the freshly committed projection here -- the reconciliation the caret
    // bug lives in.
    fn complete(&mut self, cx: &mut TestAppContext) {
      let Some(in_flight) = self.in_flight.take() else {
        return;
      };
      let commit = self
        .runtime
        .apply_editor_commands(in_flight.txn, &in_flight.base, &in_flight.commands, in_flight.selection_after.as_ref())
        .expect("apply_editor_commands must succeed for preflighted optimistic commands");
      let EditorCommitResult { new_frontier, events, .. } = commit;
      for event in events {
        cx.update(|cx| {
          self.entity.update(cx, |editor, cx| match event {
            RuntimeEvent::ProjectionPatched { batch, .. } => {
              editor.apply_projection_patch_batch(&batch, cx).expect("apply_projection_patch_batch");
            },
            RuntimeEvent::ProjectionUpdated { document, .. } | RuntimeEvent::RevisionOpened { document, .. } => {
              editor.replace_document_projection_replaying_pending(*document, Vec::new(), None, cx);
            },
            _ => {},
          });
        });
      }
      cx.update(|cx| {
        self.entity.update(cx, |editor, cx| {
          editor
            .complete_runtime_transaction(in_flight.txn, new_frontier, None, cx)
            .expect("complete_runtime_transaction");
        });
      });
    }

    // Synchronous baseline flush: begin then immediately complete, so no edit is
    // ever queued behind an in-flight transaction.
    fn flush_fully(&mut self, cx: &mut TestAppContext) {
      if self.begin(cx) {
        self.complete(cx);
      }
    }

    // Bring the harness to a quiescent state: complete any in-flight transaction
    // and flush every remaining pending edit.
    fn drain(&mut self, cx: &mut TestAppContext) {
      let mut guard = 0;
      loop {
        guard += 1;
        assert!(guard < 64, "runtime flush drain did not converge");
        if self.in_flight.is_some() {
          self.complete(cx);
          continue;
        }
        if self.begin(cx) {
          self.complete(cx);
          continue;
        }
        break;
      }
    }

    // The visible caret (anchor/head offsets) and paragraph text the user sees.
    fn snapshot(&self, cx: &mut TestAppContext) -> (DocumentOffset, DocumentOffset, Vec<String>) {
      cx.update(|cx| {
        let editor = self.entity.read(cx);
        let selection = editor.selection();
        let document = editor.document();
        let texts: Vec<String> = (0..document.paragraphs.len()).map(|ix| paragraph_text(document, ix)).collect();
        (selection.anchor, selection.head, texts)
      })
    }
  }

  // Mirror of the app's flatten_runtime_edit_commands: concatenate the queued
  // batches' semantic commands, take the first non-empty batch's base frontier,
  // and the last recorded post-selection. Coalescing is omitted (it changes
  // neither resulting text nor caret).
  fn flatten(edits: Vec<SemanticCommandBatch>, fallback_txn: &mut u128) -> (u128, Vec<u8>, Vec<SemanticEditCommand>, Option<EditorSelection>) {
    let mut transaction_id = None;
    let mut base_frontier = None;
    let mut commands = Vec::new();
    let mut selection_after = None;
    for edit in edits {
      if transaction_id.is_none() && edit.transaction_id != 0 {
        transaction_id = Some(edit.transaction_id);
      }
      if !edit.semantic_commands.is_empty() && base_frontier.is_none() {
        base_frontier = Some(edit.base_frontier.clone());
      }
      commands.extend(edit.semantic_commands);
      if edit.selection_after.is_some() {
        selection_after = edit.selection_after;
      }
    }
    let transaction_id = transaction_id.unwrap_or_else(|| {
      let value = *fallback_txn;
      *fallback_txn = fallback_txn.wrapping_add(1).max(1);
      value
    });
    (transaction_id, base_frontier.unwrap_or_default(), commands, selection_after)
  }

  // Deterministic op generator biased toward the reported reproduction (single
  // chars, Enter, Backspace/Delete) with interspersed caret motion, selection
  // replacement, and a formatting toggle.
  fn gen_op(prng: &mut Prng) -> Op {
    const ALPHABET: &[u8; 26] = b"abcdefghijklmnopqrstuvwxyz";
    match prng.below(100) {
      0..=44 => Op::Insert(char::from(ALPHABET[prng.below(26) as usize])),
      45..=64 => Op::Enter,
      65..=77 => Op::Backspace,
      78..=84 => Op::Delete,
      85..=89 => Op::DocStart,
      90..=92 => Op::DocEnd,
      93..=95 => Op::WordLeft,
      96..=97 => Op::WordRight,
      98 => Op::SelectAll,
      _ => Op::ToggleUnderline,
    }
  }

  // Compare the subject to the shadow (reference) and drain the armed fidelity
  // violation sink, failing with full context (the ring-buffer trail is already
  // dumped by violation()) the moment either signal fires.
  fn assert_convergent(cx: &mut TestAppContext, subject: &Harness, reference: &Harness, label: &str) {
    let violations = fidelity::take_violations();
    let (s_anchor, s_head, s_text) = subject.snapshot(cx);
    let (r_anchor, r_head, r_text) = reference.snapshot(cx);
    let diverged = s_head != r_head || s_anchor != r_anchor || s_text != r_text;
    assert!(
      violations.is_empty() && !diverged,
      "{label}: diverged={diverged}\n  subject   caret anchor={s_anchor:?} head={s_head:?}\n  reference caret anchor={r_anchor:?} head={r_head:?}\n  \
         subject   text={s_text:?}\n  reference text={r_text:?}\n  fidelity violations ({}):\n{}",
      violations.len(),
      violations.join("\n"),
    );
  }

  #[gpui::test]
  fn caret_soak_optimistic_reconcile_race_holds_invariants(cx: &mut gpui::TestAppContext) {
    let _serial = serial();
    fidelity::set_enabled(true);
    // Clear any residue left on this thread by an earlier test.
    let _ = fidelity::take_violations();

    let mut subject = Harness::new(cx, "Caret soak");
    let mut reference = Harness::new(cx, "Caret soak");
    let mut prng = Prng::new(0xC0FF_EE12_3456_789A);

    for step in 0..300 {
      let op = gen_op(&mut prng);

      // Both editors receive the identical operation.
      subject.apply(cx, op);
      reference.apply(cx, op);

      // Reference is the purely-synchronous baseline: flush every op immediately.
      reference.flush_fully(cx);

      // Subject races: keep exactly one transaction in flight and let newer edits
      // queue behind it. Bias toward holding the in-flight open across several
      // ops (so keystrokes really do outrun the acknowledged transaction) and
      // only occasionally complete it.
      let roll = prng.below(3);
      let racing = subject.in_flight.is_some();
      if racing && roll == 0 {
        subject.complete(cx);
      } else if !racing && roll != 0 {
        subject.begin(cx);
      }

      assert_convergent(cx, &subject, &reference, &format!("SOAK step {step} op {op:?}"));
    }

    // Quiesce and assert the subject converged to the shadow exactly.
    subject.drain(cx);
    reference.drain(cx);
    assert_convergent(cx, &subject, &reference, "SOAK final");

    fidelity::set_enabled(false);
  }

  // Minimal reproduction of the reported bug: spam Enter+char through the real
  // flush loop with a keystroke pair queued behind an in-flight transaction on
  // every iteration, and assert the caret never regresses and no fidelity
  // invariant fires.
  #[gpui::test]
  fn enter_char_spam_never_regresses_caret(cx: &mut gpui::TestAppContext) {
    let _serial = serial();
    fidelity::set_enabled(true);
    let _ = fidelity::take_violations();

    let mut subject = Harness::new(cx, "Enter spam");
    let mut reference = Harness::new(cx, "Enter spam");

    let mut prev_head = DocumentOffset::default();
    for iteration in 0..80 {
      // Scope the firehose to this iteration so a failure prints just its trail.
      let _ = fidelity::drain_ring();
      // Open the in-flight gate from the previous iteration's queued edits, so
      // the Enter+char typed below reconcile behind an outstanding transaction.
      subject.begin(cx);

      for op in [Op::Enter, Op::Insert('x')] {
        subject.apply(cx, op);
        reference.apply(cx, op);
        reference.flush_fully(cx);
      }

      // Resolve the outstanding transaction, replaying the queued Enter+char.
      subject.complete(cx);

      let violations = fidelity::take_violations();
      let trail = fidelity::drain_ring();
      // The Enter+char batch must reproduce the authoritative projection via an
      // incremental patch, NOT force a full-snapshot fallback (a perf regression
      // guard: a lossy structural diff would trip `patch-verify-fallback`).
      let took_full_snapshot = trail.iter().any(|line| line.contains("patch-verify-fallback"));
      let (s_anchor, s_head, s_text) = subject.snapshot(cx);
      let (r_anchor, r_head, r_text) = reference.snapshot(cx);
      // Caret must equal the synchronous shadow, must not regress against the
      // previous iteration (the document only grows here), and no invariant
      // fired.
      let regressed = s_head.paragraph < prev_head.paragraph || (s_head.paragraph == prev_head.paragraph && s_head.byte < prev_head.byte);
      assert!(
        violations.is_empty() && s_head == r_head && s_anchor == r_anchor && s_text == r_text && !regressed && !took_full_snapshot,
        "FAST iteration {iteration}: regressed={regressed} took_full_snapshot={took_full_snapshot} prev_head={prev_head:?}\n  subject   caret anchor={s_anchor:?} head={s_head:?}\n  \
           reference caret anchor={r_anchor:?} head={r_head:?}\n  subject   text={s_text:?}\n  reference text={r_text:?}\n  \
           fidelity violations ({}):\n{}\n  --- event trail ---\n{}",
        violations.len(),
        violations.join("\n"),
        trail.join("\n"),
      );
      prev_head = s_head;
    }

    subject.drain(cx);
    assert_convergent(cx, &subject, &reference, "FAST final");

    fidelity::set_enabled(false);
  }
}
