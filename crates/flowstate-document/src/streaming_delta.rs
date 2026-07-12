//! §act-eleven A11.10 (perf-heaven T7.7): streaming body-delta
//! materialization. `LoroText::to_delta()` builds the whole body TWICE — the
//! handler materializes a `Vec<LoroValue>` (a map per segment: `"insert"`
//! string + `"attributes"` map), then the `loro` facade copies every string
//! and attribute map AGAIN into `Vec<TextDelta>`. [`streaming_to_delta`]
//! builds the `Vec<TextDelta>` directly from the vendored streaming span
//! walker (`TextHandler::for_each_richtext_span`), skipping the `LoroValue`
//! intermediate entirely: one string copy per segment, one attribute map per
//! style CHANGE (not per span).
//!
//! Equivalence is pinned two ways: the vendored value functions are expressed
//! on the same walkers (so the T1 verify + corpus + fuzz oracles pin the walk
//! itself), and the tests below assert `streaming_to_delta == to_delta`
//! segment-for-segment on styled/unmarked/fragmented fixtures — including the
//! decoded-snapshot `LazyLoad::Src` fast path.

use loro::{ContainerTrait as _, LoroText, TextDelta};
use loro_internal::delta::StyleMeta;

/// Drop-in replacement for `text.to_delta()` (insert-only segments, adjacent
/// spans merged on visible-attribute equality — the exact merge law
/// `get_richtext_value` applies).
pub fn streaming_to_delta(text: &LoroText) -> Vec<TextDelta> {
  let handler = text.to_handler();
  let mut out: Vec<TextDelta> = Vec::new();
  let mut last_meta: Option<StyleMeta> = None;
  handler.for_each_richtext_span(&mut |chunk, meta| {
    if let Some(last) = last_meta.as_ref()
      && last.visible_eq(meta)
      && let Some(TextDelta::Insert { insert, .. }) = out.last_mut()
    {
      insert.push_str(chunk);
      return;
    }
    let visible = meta.to_visible_attributes();
    out.push(TextDelta::Insert {
      insert: chunk.to_string(),
      attributes: (!visible.is_empty()).then_some(visible),
    });
    last_meta = Some(meta.clone());
  });
  out
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::loro_schema::{
    self, MARK_DIRECT_UNDERLINE, MARK_HIGHLIGHT_STYLE, MARK_RUN_SEMANTIC_STYLE, MARK_STRIKETHROUGH,
  };
  use loro::{LoroDoc, LoroValue};

  /// A delta-shape-diverse fixture: several paragraphs, overlapping marks of
  /// every run-style key, an UNMARK inside a marked range (null-valued style —
  /// the visible-equality edge), fragmented same-style chunks (out-of-order
  /// inserts), and multi-byte text.
  fn styled_doc() -> (LoroDoc, LoroText) {
    let doc = LoroDoc::new();
    loro_schema::configure_text_styles(&doc);
    let text = doc.get_text("streaming-fixture");
    text.insert(0, "Plain lead-in with 宽字符 content.\nSecond paragraph body.\nThird row.")
      .expect("insert");
    // Fragment the chunk tree: a later insert in the MIDDLE of the first run
    // splits it into multiple same-style chunks that the delta must re-merge.
    text.insert(5, " spliced").expect("mid insert");
    text.mark(0..12, MARK_RUN_SEMANTIC_STYLE, LoroValue::I64(2)).expect("mark semantic");
    text.mark(8..20, MARK_HIGHLIGHT_STYLE, LoroValue::I64(1)).expect("mark highlight");
    text.mark(30..44, MARK_DIRECT_UNDERLINE, LoroValue::Bool(true)).expect("mark underline");
    text.mark(38..50, MARK_STRIKETHROUGH, LoroValue::Bool(true)).expect("mark strike");
    // Unmark a subrange INSIDE the semantic run: writes a null-valued style
    // key, which the delta's attribute maps must treat as absent.
    text.unmark(4..9, MARK_RUN_SEMANTIC_STYLE).expect("unmark");
    doc.commit();
    (doc, text)
  }

  #[test]
  fn streaming_matches_to_delta_on_built_state() {
    let (_doc, text) = styled_doc();
    // to_delta first: forces/uses the built state; streaming then walks the
    // same Dst tree.
    let baseline = text.to_delta();
    let streamed = streaming_to_delta(&text);
    assert!(!baseline.is_empty(), "fixture produced an empty delta — net is vacuous");
    assert_eq!(streamed, baseline, "streaming delta diverged from to_delta on the built state");
  }

  #[test]
  fn streaming_matches_to_delta_on_decoded_snapshot_src_path() {
    let (doc, _text) = styled_doc();
    let snapshot = doc.export(loro::ExportMode::Snapshot).expect("snapshot export");
    // Streaming FIRST on the freshly decoded doc: the richtext state is still
    // `LazyLoad::Src`, so this exercises `for_each_span_from_src`.
    let reloaded = LoroDoc::new();
    reloaded.import(&snapshot).expect("snapshot import");
    let streamed = streaming_to_delta(&reloaded.get_text("streaming-fixture"));
    // Baseline from a SEPARATE decode so neither call can warm the other.
    let baseline_doc = LoroDoc::new();
    baseline_doc.import(&snapshot).expect("snapshot import");
    let baseline = baseline_doc.get_text("streaming-fixture").to_delta();
    assert!(!baseline.is_empty(), "fixture produced an empty delta — net is vacuous");
    assert_eq!(streamed, baseline, "streaming delta diverged from to_delta on the Src fast path");
  }

  #[test]
  fn streaming_on_empty_text_is_empty() {
    let doc = LoroDoc::new();
    let text = doc.get_text("empty");
    assert_eq!(streaming_to_delta(&text), Vec::<TextDelta>::new());
  }
}
