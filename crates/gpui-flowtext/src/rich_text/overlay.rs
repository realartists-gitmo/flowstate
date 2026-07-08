//! §act-three A — the view-overlay layer (lever 6).
//!
//! The visible document is a **pure, recomputable view** over the canonical
//! projection:
//!
//! ```text
//! visible = canonical ⊕ overlay(pending_intent_queue)
//! ```
//!
//! An [`OverlayQueue`] holds the local intents that have been submitted but not
//! yet acknowledged by a drain of the ordered projection stream. Each render,
//! [`OverlayQueue::derive_visible`] clones the canonical projection and folds
//! the queued intents' **editor-side predictions** on top of it, in queue
//! order. Predictions reuse the act-one in-place `edit_ops` primitives, so they
//! are µs-scale and *visually* correct; they are NOT canonically exact
//! (fabricated ids, coalescing subtleties, and repair side-effects may differ —
//! the drain replaces them).
//!
//! The load-bearing property — and the reason this cannot reintroduce the
//! committed/visible divergence-cascade class — is that the overlay is
//! **stateless re-derivation**, never transformation or rebasing. `visible` has
//! no independent existence that can drift: it is always exactly
//! `canonical ⊕ overlay(queue)`. Therefore:
//!
//! > **Oracle 9.1 (eventual exactness).** After any drain that empties the
//! > queue, `derive_visible(canonical) == canonical`, byte-for-byte — because
//! > with an empty queue `derive_visible` is a clone of canonical with nothing
//! > folded on. A wrong prediction survives at most until its intent is
//! > acknowledged and popped. Divergence is transient *by construction*.
//!
//! Reconciliation is deletion + recomputation of a cache, never a merge of two
//! histories. The canonical projection (advanced solely by draining the ordered
//! stream, unchanged) remains the single authority.

use std::collections::VecDeque;

use crate::{
  Block, DocumentOffset, DocumentProjection, LocalIntent, ParagraphId, ParagraphStyle, RunStyles, block_ix_for_paragraph,
  delete_cross_paragraph_range, insert_text_at, mutate_runs_in_range, paragraph_text_len, split_paragraph_at,
};

/// Editor-minted correlation id for a queued intent. Monotonic per queue; used
/// to acknowledge (pop) the matching entry when its commit drains.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OverlayIntentId(pub u64);

/// Whether an intent has a high-confidence editor-side predictor. Ops without
/// one (objects, tables, rich fragments, replace-all, equation/image edits)
/// stay on the sync path and are represented in the overlay as *inert*: they
/// occupy a queue slot (so ordering + bounds hold) but contribute no prediction
/// to the visible view — the drain delivers their canonical effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Predictability {
  /// The intent was folded into the visible view.
  Predicted,
  /// The intent has a predictor but it could not resolve against the current
  /// view (stale identity / out-of-range) — skipped this derive; the canonical
  /// drain will deliver it.
  Unresolved,
  /// The intent has no editor-side predictor and stays on the sync path.
  Inert,
}

/// A submitted-but-unacknowledged local intent plus its correlation id.
#[derive(Clone, Debug)]
struct PendingIntent {
  id: OverlayIntentId,
  intent: LocalIntent,
}

/// Bounds (spec §2 A.4). Beyond either, input blocks with explicit pending UI
/// rather than letting the overlay drift; once workstreams B/D land (mass ops
/// ≤ 500 ms canonical) exceeding these should be effectively unreachable.
pub const MAX_QUEUE_DEPTH: usize = 64;

/// The result of a prediction-quality measurement over one derive (§9.2:
/// "prediction quality is measured, not assumed").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeriveStats {
  pub predicted: usize,
  pub unresolved: usize,
  pub inert: usize,
}

/// The editor-side pending-intent queue. Holds no projection state of its own;
/// `derive_visible` recomputes the view from canonical on demand.
#[derive(Clone, Debug, Default)]
pub struct OverlayQueue {
  queue: VecDeque<PendingIntent>,
  next_id: u64,
  last_stats: DeriveStats,
}

impl OverlayQueue {
  #[must_use]
  pub fn new() -> Self {
    Self::default()
  }

  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.queue.is_empty()
  }

  #[must_use]
  pub fn len(&self) -> usize {
    self.queue.len()
  }

  /// Whether a new intent may be enqueued without exceeding the depth bound.
  #[must_use]
  pub fn has_capacity(&self) -> bool {
    self.queue.len() < MAX_QUEUE_DEPTH
  }

  #[must_use]
  pub fn last_stats(&self) -> DeriveStats {
    self.last_stats
  }

  /// Enqueue an intent, returning its correlation id. The caller is expected to
  /// have checked [`Self::has_capacity`]; enqueuing past the bound still works
  /// (it never silently drops) but the caller should block input instead.
  pub fn enqueue(&mut self, intent: LocalIntent) -> OverlayIntentId {
    let id = OverlayIntentId(self.next_id);
    self.next_id = self.next_id.wrapping_add(1);
    self.queue.push_back(PendingIntent { id, intent });
    id
  }

  /// Acknowledge (pop) the intent with `id`, if present. Returns whether it was
  /// found. Local intents serialize through the gate in submission order, so
  /// acks normally arrive front-to-back; matching by id (not position) keeps
  /// the queue correct even if an intent is rejected and never acked (it is
  /// simply removed here by a later reconciliation).
  pub fn acknowledge(&mut self, id: OverlayIntentId) -> bool {
    if let Some(pos) = self.queue.iter().position(|pending| pending.id == id) {
      self.queue.remove(pos);
      true
    } else {
      false
    }
  }

  /// Acknowledge the oldest queued intent (FIFO). Since the gate serializes
  /// local intents in submission order, the oldest queued intent is the one a
  /// fresh `LocalCommit` corresponds to. Returns its id if the queue was
  /// non-empty.
  pub fn acknowledge_oldest(&mut self) -> Option<OverlayIntentId> {
    self.queue.pop_front().map(|pending| pending.id)
  }

  /// Cancel the most recently enqueued intent (pop the back). Used when an
  /// optimistically-enqueued intent is REJECTED by the authority (I-15): the
  /// speculative overlay entry must be withdrawn so it never renders.
  pub fn cancel_newest(&mut self) -> bool {
    self.queue.pop_back().is_some()
  }

  /// Drop every queued intent (spec §A.4: undo/redo, revision ops, and session
  /// join/leave flush the queue first — they must observe a quiesced
  /// authority).
  pub fn clear(&mut self) {
    self.queue.clear();
  }

  /// Re-derive the visible view: `canonical ⊕ overlay(queue)`. Clones the
  /// canonical projection (cheap — its `Vec`s are behind `Arc`) and folds each
  /// queued intent's prediction on top, in queue order. Unpredictable or
  /// unresolvable intents are skipped in the view but remain in the queue for
  /// their canonical drain. Records per-derive prediction-quality stats.
  ///
  /// With an empty queue this returns a byte-for-byte clone of `canonical`
  /// (oracle 9.1).
  #[hotpath::measure]
  pub fn derive_visible(&mut self, canonical: &DocumentProjection) -> DocumentProjection {
    if self.queue.is_empty() {
      self.last_stats = DeriveStats::default();
      return canonical.clone();
    }
    let mut visible = canonical.clone();
    let mut stats = DeriveStats::default();
    for pending in &self.queue {
      match predict_into(&mut visible, &pending.intent) {
        Predictability::Predicted => stats.predicted += 1,
        Predictability::Unresolved => stats.unresolved += 1,
        Predictability::Inert => stats.inert += 1,
      }
    }
    self.last_stats = stats;
    visible
  }
}

/// Resolve a paragraph identity against the in-progress view.
fn resolve_paragraph(document: &DocumentProjection, id: ParagraphId) -> Option<usize> {
  document.ids.paragraph_ids.iter().position(|candidate| *candidate == id)
}

/// Clamp a byte hint to the paragraph's current length (predictions run against
/// the evolving view, so a stale hint from the submitting projection must not
/// panic the slice math).
fn clamp_byte(document: &DocumentProjection, paragraph_ix: usize, byte: usize) -> usize {
  document.paragraphs.get(paragraph_ix).map(paragraph_text_len).unwrap_or(0).min(byte)
}

/// Fold ONE intent's editor-side prediction into `visible`. Returns how it was
/// handled. Predictable classes cover the latency-sensitive text/style ops; the
/// rest are inert (sync-path) and contribute nothing to the overlay.
fn predict_into(visible: &mut DocumentProjection, intent: &LocalIntent) -> Predictability {
  match intent {
    LocalIntent::InsertText(insert) => {
      let Some(paragraph_ix) = resolve_paragraph(visible, insert.at.paragraph) else {
        return Predictability::Unresolved;
      };
      let byte = clamp_byte(visible, paragraph_ix, insert.at.byte_hint);
      let styles = insert.style_override.unwrap_or_else(|| style_at(visible, paragraph_ix, byte));
      insert_text_at(visible, paragraph_ix, byte, &insert.text, styles);
      Predictability::Predicted
    },
    LocalIntent::DeleteRange(delete) => {
      let (Some(start_ix), Some(end_ix)) = (resolve_paragraph(visible, delete.start.paragraph), resolve_paragraph(visible, delete.end.paragraph))
      else {
        return Predictability::Unresolved;
      };
      let start = DocumentOffset {
        paragraph: start_ix,
        byte: clamp_byte(visible, start_ix, delete.start.byte_hint),
      };
      let end = DocumentOffset {
        paragraph: end_ix,
        byte: clamp_byte(visible, end_ix, delete.end.byte_hint),
      };
      if start.paragraph > end.paragraph || (start.paragraph == end.paragraph && start.byte >= end.byte) {
        return Predictability::Unresolved;
      }
      delete_cross_paragraph_range(visible, start..end);
      Predictability::Predicted
    },
    LocalIntent::SplitParagraph(split) => {
      let Some(paragraph_ix) = resolve_paragraph(visible, split.at.paragraph) else {
        return Predictability::Unresolved;
      };
      let byte = clamp_byte(visible, paragraph_ix, split.at.byte_hint);
      split_paragraph_at(visible, paragraph_ix, byte);
      Predictability::Predicted
    },
    LocalIntent::JoinParagraphs(join) => {
      let (Some(first_ix), Some(second_ix)) = (resolve_paragraph(visible, join.first), resolve_paragraph(visible, join.second)) else {
        return Predictability::Unresolved;
      };
      // Adjacent-only join (the intent contract); express it as deleting the
      // boundary between the end of `first` and the start of `second`.
      if second_ix != first_ix + 1 {
        return Predictability::Unresolved;
      }
      let first_len = visible.paragraphs.get(first_ix).map(paragraph_text_len).unwrap_or(0);
      let start = DocumentOffset {
        paragraph: first_ix,
        byte: first_len,
      };
      let end = DocumentOffset { paragraph: second_ix, byte: 0 };
      delete_cross_paragraph_range(visible, start..end);
      Predictability::Predicted
    },
    LocalIntent::SetMarks(marks) => {
      let (Some(start_ix), Some(end_ix)) = (resolve_paragraph(visible, marks.start.paragraph), resolve_paragraph(visible, marks.end.paragraph)) else {
        return Predictability::Unresolved;
      };
      let start = DocumentOffset {
        paragraph: start_ix,
        byte: clamp_byte(visible, start_ix, marks.start.byte_hint),
      };
      let end = DocumentOffset {
        paragraph: end_ix,
        byte: clamp_byte(visible, end_ix, marks.end.byte_hint),
      };
      let styles = marks.styles;
      mutate_runs_in_range(visible, start..end, |run| *run = styles);
      Predictability::Predicted
    },
    LocalIntent::SetParagraphStyle(set) => {
      let Some(paragraph_ix) = resolve_paragraph(visible, set.paragraph) else {
        return Predictability::Unresolved;
      };
      set_paragraph_style(visible, paragraph_ix, set.style);
      Predictability::Predicted
    },
    LocalIntent::SetParagraphStyles(set) => {
      let mut any = false;
      for id in &set.paragraphs {
        if let Some(paragraph_ix) = resolve_paragraph(visible, *id) {
          set_paragraph_style(visible, paragraph_ix, set.style);
          any = true;
        }
      }
      if any { Predictability::Predicted } else { Predictability::Unresolved }
    },
    // No high-confidence editor-side predictor — stays sync (spec §A.2). The
    // slot is still tracked (ordering + bounds), but nothing is folded.
    LocalIntent::InsertObject(_)
    | LocalIntent::ReplaceObject(_)
    | LocalIntent::DeleteBlocks(_)
    | LocalIntent::MoveBlock(_)
    | LocalIntent::InsertRichFragment(_)
    | LocalIntent::ReplaceMatches(_)
    | LocalIntent::ReplaceEquationSourceRange(_)
    | LocalIntent::ReplaceImageAltText(_)
    | LocalIntent::ReplaceImageCaption(_)
    | LocalIntent::SetImageLayout(_)
    | LocalIntent::Table(_) => Predictability::Inert,
  }
}

/// The run style active at a byte position (insertion inherits it when no
/// explicit override is given — matches expand-`After` semantics visually).
fn style_at(document: &DocumentProjection, paragraph_ix: usize, byte: usize) -> RunStyles {
  let Some(paragraph) = document.paragraphs.get(paragraph_ix) else {
    return RunStyles::default();
  };
  let mut offset = 0;
  let mut last = RunStyles::default();
  for run in &paragraph.runs {
    let run_end = offset + run.len;
    if byte <= run_end && byte >= offset {
      return run.styles;
    }
    last = run.styles;
    offset = run_end;
  }
  last
}

/// Set a paragraph's style, keeping the mirrored `Block::Paragraph` copy in
/// sync (the render reads paragraphs; the block copy backs structural patches).
fn set_paragraph_style(document: &mut DocumentProjection, paragraph_ix: usize, style: ParagraphStyle) {
  let Some(paragraph) = document.paragraphs.get_mut(paragraph_ix) else {
    return;
  };
  paragraph.style = style;
  paragraph.version = paragraph.version.wrapping_add(1);
  if let Some(block_ix) = block_ix_for_paragraph(document, paragraph_ix) {
    let mut blocks = document.blocks.make_mut();
    if let Some(Block::Paragraph(block_paragraph)) = blocks.get_mut(block_ix) {
      block_paragraph.style = style;
      block_paragraph.version = block_paragraph.version.wrapping_add(1);
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{DocumentTheme, InputParagraph, RunSemanticStyle, TextAnchor, document_from_input, paragraph_text, plain};
  use crate::local_intents::{
    DeleteRangeIntent, InsertTextIntent, JoinParagraphsIntent, SetMarksIntent, SetParagraphStyleIntent, SetParagraphStylesIntent,
    SplitParagraphIntent,
  };

  fn doc(paragraphs: &[&str]) -> DocumentProjection {
    let paras = paragraphs
      .iter()
      .map(|text| InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain(text)],
      })
      .collect();
    document_from_input(DocumentTheme::default(), paras)
  }

  /// VISUAL equality: text, paragraph count, per-paragraph text + style + runs.
  /// Excludes the identity vectors — predictions fabricate fresh ids each
  /// derive (spec-approved; the drain replaces them), so two derives of the
  /// same queue are visually identical but carry different fabricated ids.
  fn visuals_match(a: &DocumentProjection, b: &DocumentProjection) -> bool {
    a.text == b.text
      && a.paragraphs.len() == b.paragraphs.len()
      && (0..a.paragraphs.len()).all(|ix| {
        paragraph_text(a, ix) == paragraph_text(b, ix)
          && a.paragraphs[ix].style == b.paragraphs[ix].style
          && a.paragraphs[ix].runs == b.paragraphs[ix].runs
      })
  }

  /// Full equality INCLUDING identity vectors. Valid for the oracle: an empty
  /// queue makes `derive_visible` a pure clone of canonical, so the ids match
  /// exactly.
  fn projections_match(a: &DocumentProjection, b: &DocumentProjection) -> bool {
    visuals_match(a, b) && a.ids.paragraph_ids == b.ids.paragraph_ids && a.ids.block_ids == b.ids.block_ids
  }

  fn anchor(document: &DocumentProjection, paragraph_ix: usize, byte: usize) -> TextAnchor {
    TextAnchor::new(document.ids.paragraph_ids[paragraph_ix], byte)
  }

  // ---- Oracle 9.1: empty queue ⇒ visible == canonical ---------------------

  #[test]
  fn empty_queue_yields_canonical_byte_for_byte() {
    let canonical = doc(&["alpha", "beta", "gamma"]);
    let mut overlay = OverlayQueue::new();
    let visible = overlay.derive_visible(&canonical);
    assert!(projections_match(&visible, &canonical), "empty overlay must equal canonical");
    assert_eq!(overlay.last_stats(), DeriveStats::default());
  }

  #[test]
  fn ack_to_empty_restores_canonical() {
    let canonical = doc(&["alpha", "beta"]);
    let mut overlay = OverlayQueue::new();
    let id = overlay.enqueue(LocalIntent::InsertText(InsertTextIntent {
      at: anchor(&canonical, 0, 5),
      text: "XYZ".into(),
      style_override: None,
    }));
    let visible = overlay.derive_visible(&canonical);
    assert_eq!(paragraph_text(&visible, 0), "alphaXYZ", "prediction must render immediately");
    // Ack the intent (canonical unchanged in this unit — the point is the pop).
    assert!(overlay.acknowledge(id));
    let visible = overlay.derive_visible(&canonical);
    assert!(projections_match(&visible, &canonical), "after ack the overlay must vanish (oracle 9.1)");
  }

  // ---- Prediction correctness per class -----------------------------------

  #[test]
  fn insert_predicts_text_and_inherits_style() {
    let canonical = doc(&["hello world"]);
    let mut overlay = OverlayQueue::new();
    overlay.enqueue(LocalIntent::InsertText(InsertTextIntent {
      at: anchor(&canonical, 0, 5),
      text: " there".into(),
      style_override: None,
    }));
    let visible = overlay.derive_visible(&canonical);
    assert_eq!(paragraph_text(&visible, 0), "hello there world");
    assert_eq!(overlay.last_stats().predicted, 1);
  }

  #[test]
  fn split_predicts_new_paragraph() {
    let canonical = doc(&["abcdef"]);
    let mut overlay = OverlayQueue::new();
    overlay.enqueue(LocalIntent::SplitParagraph(SplitParagraphIntent {
      at: anchor(&canonical, 0, 3),
      inherited_style: ParagraphStyle::Normal,
    }));
    let visible = overlay.derive_visible(&canonical);
    assert_eq!(visible.paragraphs.len(), 2);
    assert_eq!(paragraph_text(&visible, 0), "abc");
    assert_eq!(paragraph_text(&visible, 1), "def");
  }

  #[test]
  fn cross_paragraph_delete_predicts_merge() {
    let canonical = doc(&["first", "second", "third"]);
    let mut overlay = OverlayQueue::new();
    overlay.enqueue(LocalIntent::DeleteRange(DeleteRangeIntent {
      start: anchor(&canonical, 0, 2),
      end: anchor(&canonical, 2, 3),
    }));
    let visible = overlay.derive_visible(&canonical);
    assert_eq!(visible.paragraphs.len(), 1);
    assert_eq!(paragraph_text(&visible, 0), "fird");
  }

  #[test]
  fn join_predicts_merge() {
    let canonical = doc(&["foo", "bar"]);
    let mut overlay = OverlayQueue::new();
    overlay.enqueue(LocalIntent::JoinParagraphs(JoinParagraphsIntent {
      first: canonical.ids.paragraph_ids[0],
      second: canonical.ids.paragraph_ids[1],
    }));
    let visible = overlay.derive_visible(&canonical);
    assert_eq!(visible.paragraphs.len(), 1);
    assert_eq!(paragraph_text(&visible, 0), "foobar");
  }

  #[test]
  fn set_marks_predicts_style_run() {
    let canonical = doc(&["styled text"]);
    let mut overlay = OverlayQueue::new();
    let styles = RunStyles {
      semantic: RunSemanticStyle::Custom(2),
      ..RunStyles::default()
    };
    overlay.enqueue(LocalIntent::SetMarks(SetMarksIntent {
      start: anchor(&canonical, 0, 0),
      end: anchor(&canonical, 0, 6),
      styles,
    }));
    let visible = overlay.derive_visible(&canonical);
    assert!(visible.paragraphs[0].runs.iter().any(|run| run.styles.semantic == RunSemanticStyle::Custom(2)));
  }

  #[test]
  fn set_paragraph_styles_predicts_all() {
    let canonical = doc(&["a", "b", "c"]);
    let mut overlay = OverlayQueue::new();
    overlay.enqueue(LocalIntent::SetParagraphStyles(SetParagraphStylesIntent {
      paragraphs: canonical.ids.paragraph_ids.clone(),
      style: ParagraphStyle::Custom(3),
    }));
    let visible = overlay.derive_visible(&canonical);
    assert!(visible.paragraphs.iter().all(|paragraph| paragraph.style == ParagraphStyle::Custom(3)));
    // The mirrored block copies must agree (structural patches read them).
    assert!(visible.blocks.iter().all(|block| match block {
      Block::Paragraph(paragraph) => paragraph.style == ParagraphStyle::Custom(3),
      _ => true,
    }));
  }

  #[test]
  fn single_paragraph_style_predicts() {
    let canonical = doc(&["x", "y"]);
    let mut overlay = OverlayQueue::new();
    overlay.enqueue(LocalIntent::SetParagraphStyle(SetParagraphStyleIntent {
      paragraph: canonical.ids.paragraph_ids[1],
      style: ParagraphStyle::Custom(1),
    }));
    let visible = overlay.derive_visible(&canonical);
    assert_eq!(visible.paragraphs[1].style, ParagraphStyle::Custom(1));
    assert_eq!(visible.paragraphs[0].style, ParagraphStyle::Normal);
  }

  // ---- Composition: predictions stack in queue order ----------------------

  #[test]
  fn predictions_compose_in_queue_order() {
    let canonical = doc(&["base"]);
    let mut overlay = OverlayQueue::new();
    // Type "base" -> "baseX" -> split at 5 -> "baseX" | "".
    overlay.enqueue(LocalIntent::InsertText(InsertTextIntent {
      at: anchor(&canonical, 0, 4),
      text: "X".into(),
      style_override: None,
    }));
    // The split targets the SAME paragraph id, now longer in the view.
    overlay.enqueue(LocalIntent::SplitParagraph(SplitParagraphIntent {
      at: anchor(&canonical, 0, 5),
      inherited_style: ParagraphStyle::Normal,
    }));
    let visible = overlay.derive_visible(&canonical);
    assert_eq!(visible.paragraphs.len(), 2);
    assert_eq!(paragraph_text(&visible, 0), "baseX");
    assert_eq!(paragraph_text(&visible, 1), "");
    assert_eq!(overlay.last_stats().predicted, 2);
  }

  // ---- Inert (sync-path) ops occupy a slot but do not render --------------

  #[test]
  fn unresolved_intent_is_skipped_not_panicking() {
    let canonical = doc(&["only"]);
    let mut overlay = OverlayQueue::new();
    // A stale identity that does not exist in the view.
    overlay.enqueue(LocalIntent::InsertText(InsertTextIntent {
      at: TextAnchor::new(ParagraphId(0xDEAD), 0),
      text: "ghost".into(),
      style_override: None,
    }));
    let visible = overlay.derive_visible(&canonical);
    assert!(projections_match(&visible, &canonical), "an unresolvable prediction contributes nothing");
    assert_eq!(overlay.last_stats().unresolved, 1);
  }

  // ---- Bounds --------------------------------------------------------------

  #[test]
  fn capacity_bound_is_reported() {
    let mut overlay = OverlayQueue::new();
    let canonical = doc(&["p"]);
    for _ in 0..MAX_QUEUE_DEPTH {
      assert!(overlay.has_capacity());
      overlay.enqueue(LocalIntent::InsertText(InsertTextIntent {
        at: anchor(&canonical, 0, 0),
        text: "a".into(),
        style_override: None,
      }));
    }
    assert!(!overlay.has_capacity(), "queue at MAX_QUEUE_DEPTH must report full");
  }

  // ---- Model check: exhaustive queue-lifecycle enumeration ----------------
  //
  // The overlay state machine is (queue configuration × drain events × remote
  // events). We enumerate all operation sequences up to a bounded depth over a
  // small alphabet and assert the two load-bearing invariants after EVERY
  // step:
  //   (I1) determinism — deriving twice from the same (canonical, queue)
  //        yields byte-identical views;
  //   (I2) oracle 9.1 — whenever the queue is empty, visible == canonical,
  //        no matter what canonical the interleaved "remote" drains produced.
  // This closes the interleaving-sampling gap exactly where the persistence-of
  // -divergence property could hide.

  #[derive(Clone, Copy)]
  enum Op {
    EnqueueInsert,
    EnqueueSplit,
    EnqueueInert,
    AckOldest,
    RemoteEdit,
  }

  fn apply_remote_edit(canonical: &mut DocumentProjection, step: usize) {
    // A canonical-only change (as a remote import would produce): append a
    // char to the first paragraph. This is the "canonical moved under the
    // overlay" axis.
    let ix = 0;
    let byte = paragraph_text(canonical, ix).len();
    insert_text_at(canonical, ix, byte, &format!("r{}", step % 10), RunStyles::default());
  }

  fn run_model_sequence(ops: &[Op]) {
    let mut canonical = doc(&["seed alpha", "seed beta"]);
    let mut overlay = OverlayQueue::new();
    let base_id = canonical.ids.paragraph_ids[0];
    for (step, op) in ops.iter().enumerate() {
      match op {
        Op::EnqueueInsert if overlay.has_capacity() => {
          overlay.enqueue(LocalIntent::InsertText(InsertTextIntent {
            at: TextAnchor::new(base_id, 1),
            text: format!("i{}", step % 10),
            style_override: None,
          }));
        },
        Op::EnqueueSplit if overlay.has_capacity() => {
          overlay.enqueue(LocalIntent::SplitParagraph(SplitParagraphIntent {
            at: TextAnchor::new(base_id, 1),
            inherited_style: ParagraphStyle::Normal,
          }));
        },
        Op::EnqueueInert if overlay.has_capacity() => {
          // A predictor-less class occupies a slot without folding.
          overlay.enqueue(LocalIntent::ReplaceImageAltText(crate::local_intents::ReplaceImageAltTextIntent {
            image: crate::BlockId(1),
            text: "alt".into(),
          }));
        },
        Op::AckOldest => {
          let _ = overlay.acknowledge_oldest();
        },
        Op::RemoteEdit => apply_remote_edit(&mut canonical, step),
        _ => {},
      }
      // (I1) VISUAL determinism — two derives of the same (canonical, queue)
      // render identically (fabricated ids may differ, spec-approved).
      let v1 = overlay.derive_visible(&canonical);
      let v2 = overlay.derive_visible(&canonical);
      assert!(visuals_match(&v1, &v2), "derive must be visually deterministic");
      // (I2) oracle 9.1 — empty queue ⇒ visible == canonical byte-for-byte,
      // ids included (pure clone).
      if overlay.is_empty() {
        assert!(
          projections_match(&v1, &canonical),
          "oracle 9.1: empty queue must equal canonical (ops so far len {})",
          ops.len()
        );
      }
    }
  }

  #[test]
  fn model_check_overlay_lifecycle_exhaustive() {
    const ALPHABET: [Op; 5] = [Op::EnqueueInsert, Op::EnqueueSplit, Op::EnqueueInert, Op::AckOldest, Op::RemoteEdit];
    // Enumerate all sequences up to depth D over the 5-symbol alphabet
    // (5^D states). D=6 → 15625 sequences, each checked step-by-step.
    const DEPTH: usize = 6;
    let mut sequence = Vec::with_capacity(DEPTH);
    fn recurse(sequence: &mut Vec<Op>, depth: usize) {
      if depth == 0 {
        run_model_sequence(sequence);
        return;
      }
      for op in ALPHABET {
        sequence.push(op);
        recurse(sequence, depth - 1);
        sequence.pop();
      }
    }
    recurse(&mut sequence, DEPTH);
    let _ = &ALPHABET;
    let _ = &mut sequence;
  }
}
