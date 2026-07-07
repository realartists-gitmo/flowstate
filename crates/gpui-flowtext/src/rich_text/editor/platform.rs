#[cfg(target_os = "windows")]
#[hotpath::measure]
fn windows_apply_capslock(text: &str) -> String {
  // GPUI 0.2.2's Windows key_char generation does not include Caps Lock in
  // the ToUnicode keyboard state. For normal letter input, Caps Lock inverts
  // the Shift-produced case; non-letter keys should pass through unchanged.
  let mut chars = text.chars();
  let Some(ch) = chars.next() else {
    return String::new();
  };
  if chars.next().is_none() && ch.is_ascii_alphabetic() {
    if ch.is_ascii_lowercase() {
      ch.to_ascii_uppercase().to_string()
    } else {
      ch.to_ascii_lowercase().to_string()
    }
  } else {
    text.to_string()
  }
}

#[hotpath::measure_all]
impl RichTextEditor {
  pub fn ime_composition_active(&self) -> bool {
    self.ime_marked_range.is_some()
  }

  fn selection_utf16_range(&self) -> Range<usize> {
    let text = full_document_text(&self.document);
    let anchor = byte_to_utf16(
      &text,
      global_byte(&self.document, clamp_offset_to_document(&self.document, self.selection.anchor)),
    );
    let head = byte_to_utf16(
      &text,
      global_byte(&self.document, clamp_offset_to_document(&self.document, self.selection.head)),
    );
    anchor.min(head)..anchor.max(head)
  }

  fn utf16_range_to_document_offsets(&self, range: Range<usize>) -> Range<DocumentOffset> {
    let text = full_document_text(&self.document);
    let start = utf16_to_byte(&text, range.start);
    let end = utf16_to_byte(&text, range.end).max(start);
    global_to_document_offset(&self.document, start)..global_to_document_offset(&self.document, end)
  }

  /// Loro-first (spec §5): one platform text replacement = typed intents
  /// only. The UTF-16 range collapses into the selection, then the shared
  /// caret-insert law (`write_insert_text_at_caret`) performs the replacement
  /// — a non-caret selection groups its delete+insert as ONE undo group — and
  /// a bare deletion is one `DeleteRange` intent. Returns whether a document
  /// mutation committed.
  fn replace_text_for_utf16_range(&mut self, range: Range<usize>, text: &str, cx: &mut Context<Self>) -> bool {
    if self.insert_text_into_selected_object_text(text, cx) {
      return true;
    }
    let offsets = self.utf16_range_to_document_offsets(range);
    let selection = EditorSelection::range(offsets.start, offsets.end);
    let before_selection = self.selection.clone();
    self.selection = selection;
    if text.is_empty() && self.selection.is_caret() {
      if self.selection != before_selection {
        self.emit_selection_changed(cx);
        self.reset_caret_blink(cx);
        cx.notify();
      }
      return false;
    }
    let committed = if !self.can_write_collaboration() {
      cx.notify();
      false
    } else if text.is_empty() {
      self.write_delete_selection(cx)
    } else {
      self.write_insert_text_at_caret(text, cx)
    };
    if self.selection != before_selection {
      self.emit_selection_changed(cx);
    }
    committed
  }

}

#[hotpath::measure_all]
impl EntityInputHandler for RichTextEditor {
  fn text_for_range(
    &mut self,
    range: Range<usize>,
    adjusted_range: &mut Option<Range<usize>>,
    _: &mut Window,
    _: &mut Context<Self>,
  ) -> Option<String> {
    let text = full_document_text(&self.document);
    let start = utf16_to_byte(&text, range.start);
    let end = utf16_to_byte(&text, range.end).max(start);
    *adjusted_range = Some(byte_range_to_utf16(&text, start..end));
    Some(document_text_slice(&self.document, start..end))
  }

  fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
    Some(UTF16Selection {
      range: self.selection_utf16_range(),
      reversed: self.selection.anchor > self.selection.head,
    })
  }

  fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
    self.ime_marked_range.clone()
  }

  fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
    if self.ime_marked_range.take().is_some() {
      cx.notify();
    }
  }

  fn replace_text_in_range(&mut self, range: Option<Range<usize>>, text: &str, _: &mut Window, cx: &mut Context<Self>) {
    // Composition commit / plain replacement (spec §5): when a marked
    // (composition) range exists it is the replacement target — the shared
    // intent path deletes it (write_delete_offset_range under the hood) and
    // inserts the final text (write_insert_text_at_caret) as ONE undo group.
    // With no composition, write_insert_text_at_caret handles selection
    // replacement itself. Never a stale visual offset: anchors resolve
    // against the current projection inside the write helpers.
    let range = range
      .or_else(|| self.ime_marked_range.clone())
      .unwrap_or_else(|| self.selection_utf16_range());
    self.ime_marked_range = None;
    let _ = self.replace_text_for_utf16_range(range, text, cx);
  }

  fn replace_and_mark_text_in_range(
    &mut self,
    range: Option<Range<usize>>,
    new_text: &str,
    new_selected_range: Option<Range<usize>>,
    _: &mut Window,
    cx: &mut Context<Self>,
  ) {
    // Composition preview (spec §5, minimal conversion): every marked-text
    // update commits through typed intents as ONE undo-grouped pair — delete
    // the previous marked range, insert the new marked text — via the shared
    // caret-insert law; there is no direct projection mutation. The §5 end
    // state is a full overlay-only composition (marked text NEVER in Loro
    // until composition commit, anchored by a composition-start Loro cursor);
    // until then `ime_marked_range` is render/IME-query bookkeeping only and
    // is never used as mutation authority.
    let range = range
      .or_else(|| self.ime_marked_range.clone())
      .unwrap_or_else(|| self.selection_utf16_range());
    let mark_start = range.start;
    let committed = self.replace_text_for_utf16_range(range, new_text, cx);
    if new_text.is_empty() || !committed {
      // Nothing (or nothing new) is marked in the document — drop the marked
      // range rather than let it drift from committed state.
      self.ime_marked_range = None;
      return;
    }
    let marked_range = mark_start..mark_start + new_text.encode_utf16().count();
    self.ime_marked_range = Some(marked_range.clone());
    let selection_range = new_selected_range
      .map(|range| marked_range.start + range.start..marked_range.start + range.end)
      .unwrap_or_else(|| marked_range.end..marked_range.end);
    let offsets = self.utf16_range_to_document_offsets(selection_range);
    let selection = EditorSelection::range(offsets.start, offsets.end);
    if self.selection != selection {
      self.selection = selection;
      self.emit_selection_changed(cx);
      self.reset_caret_blink(cx);
      cx.notify();
    }
  }

  fn bounds_for_range(
    &mut self,
    range_utf16: Range<usize>,
    _: Bounds<Pixels>,
    _: &mut Window,
    _: &mut Context<Self>,
  ) -> Option<Bounds<Pixels>> {
    let offsets = self.utf16_range_to_document_offsets(range_utf16);
    let layout = self.layout_for_offset(offsets.start)?;
    let origin = layout.bounds?.origin;
    caret_bounds(&layout, offsets.start, VisualGravity::Neutral, origin)
  }

  fn character_index_for_point(&mut self, point: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) -> Option<usize> {
    let offset = self.hit_test_document_position(point, window, cx);
    let text = full_document_text(&self.document);
    Some(byte_to_utf16(
      &text,
      global_byte(&self.document, clamp_offset_to_document(&self.document, offset)),
    ))
  }
}

#[hotpath::measure]
fn byte_range_to_utf16(text: &str, range: Range<usize>) -> Range<usize> {
  byte_to_utf16(text, range.start)..byte_to_utf16(text, range.end)
}

#[hotpath::measure]
fn byte_to_utf16(text: &str, byte: usize) -> usize {
  let byte = previous_char_boundary(text, byte);
  text[..byte].encode_utf16().count()
}

#[hotpath::measure]
fn utf16_to_byte(text: &str, target: usize) -> usize {
  let mut utf16 = 0usize;
  for (byte, ch) in text.char_indices() {
    if target <= utf16 {
      return byte;
    }
    let next = utf16 + ch.len_utf16();
    if target < next {
      return byte;
    }
    utf16 = next;
  }
  text.len()
}

#[hotpath::measure]
fn previous_char_boundary(text: &str, byte: usize) -> usize {
  let mut byte = byte.min(text.len());
  while !text.is_char_boundary(byte) {
    byte = byte.saturating_sub(1);
  }
  byte
}
