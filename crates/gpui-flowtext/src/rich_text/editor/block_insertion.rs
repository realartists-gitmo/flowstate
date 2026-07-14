#[hotpath::measure_all]
impl RichTextEditor {
  pub fn insert_toolkit_text_at_caret(&mut self, paragraphs: Vec<InputParagraph>, cx: &mut Context<Self>) {
    let paragraphs = non_empty_input_paragraphs(paragraphs);
    if paragraphs.is_empty() {
      return;
    }
    let fragment = RichClipboardFragment {
      format: RICH_TEXT_CLIPBOARD_FORMAT.to_string(),
      paragraphs,
      blocks: Vec::new(),
      assets: Vec::new(),
    };
    if self.insert_rich_fragment_into_selected_table_cell(&fragment, cx) {
      return;
    }
    let _ = self.write_clipboard_fragment_at_caret(&fragment, cx);
  }

  pub fn insert_toolkit_paragraphs_as_blocks(&mut self, paragraphs: Vec<InputParagraph>, cx: &mut Context<Self>) {
    let blocks = non_empty_input_paragraphs(paragraphs)
      .into_iter()
      .map(FragmentBlock::Paragraph)
      .collect::<Vec<_>>();
    if blocks.is_empty() {
      return;
    }
    self.write_insert_rich_fragment_at_caret(blocks, cx);
  }

  /// Loro-first (spec §5): a rich fragment commits as ONE `InsertRichFragment`
  /// intent through the write authority — paragraphs and objects in order,
  /// one gate hold, one Loro commit, one undo unit. The conversion itself
  /// lives in `write_clipboard_fragment_at_caret` (paste.rs), shared with the
  /// clipboard paste path.
  fn insert_rich_fragment(&mut self, fragment: RichClipboardFragment, cx: &mut Context<Self>) {
    let _ = self.write_clipboard_fragment_at_caret(&fragment, cx);
  }

  /// Insert projection blocks at the caret through the write authority (menu
  /// insertions: tables.rs, media.rs). Object placement is the runtime's law:
  /// the object lands before the caret paragraph at byte 0, after it
  /// otherwise (`InsertObject` resolution in the write path).
  fn insert_blocks_after_caret(&mut self, blocks: Vec<Block>, cx: &mut Context<Self>) {
    let inputs = blocks.iter().map(input_block_from_block).collect::<Vec<_>>();
    let _ = self.write_input_blocks_at_caret(inputs, cx);
  }

  /// Input blocks → intents: a lone object is the precise (non-compound)
  /// `InsertObject` intent; anything else is one `InsertRichFragment`.
  pub(super) fn write_input_blocks_at_caret(&mut self, mut blocks: Vec<InputBlock>, cx: &mut Context<Self>) -> bool {
    if blocks.is_empty() {
      return false;
    }
    if blocks.len() == 1 && !matches!(blocks[0], InputBlock::Paragraph(_)) {
      let block = blocks.remove(0);
      return self.write_object_block_replacing_selection(block, cx);
    }
    let fragment_blocks = blocks
      .into_iter()
      .map(|block| match block {
        InputBlock::Paragraph(paragraph) => FragmentBlock::Paragraph(paragraph),
        block => FragmentBlock::Object(block),
      })
      .collect::<Vec<_>>();
    self.write_insert_rich_fragment_at_caret(fragment_blocks, cx)
  }

  /// One user edit = one undo group: replacing a selection with an object
  /// groups the delete with the `InsertObject` intent (mirrors
  /// `write_insert_text_at_caret`'s selection-replacement law).
  pub(super) fn write_object_block_replacing_selection(&mut self, block: InputBlock, cx: &mut Context<Self>) -> bool {
    let grouped = !self.selection.is_caret();
    if grouped {
      self.begin_undo_group();
      if !self.write_delete_selection(cx) {
        self.end_undo_group();
        return false;
      }
    }
    let committed = self.write_insert_object_at_caret(block, cx);
    if grouped {
      self.end_undo_group();
    }
    committed
  }

  /// Clipboard fragments carry raw asset bytes (images). There is no asset
  /// intent (assets are content-addressed sideband bytes, not CRDT content),
  /// so pasted bytes seed the projection's asset stores directly — the same
  /// pattern as `insert_image_assets` (media.rs) and the remote
  /// `AssetArrived` path (`projection_apply.rs`). The object blocks that
  /// reference them commit through the write authority like everything else.
  fn adopt_clipboard_assets(&mut self, assets: &[InputAsset]) {
    for asset in assets {
      let record = AssetRecord {
        id: asset.id,
        mime_type: asset.mime_type.clone().into(),
        original_name: asset.original_name.clone().map(Into::into),
        content_hash: asset.content_hash,
        bytes: Arc::new(asset.bytes.clone()),
      };
      self.document.assets.assets.insert(asset.id, record);
    }
  }

  fn insert_plain_text_fragment(&mut self, text: &str, cx: &mut Context<Self>) {
    let _ = self.write_plain_text_paste(text, cx);
  }

}

fn non_empty_input_paragraphs(paragraphs: Vec<InputParagraph>) -> Vec<InputParagraph> {
  paragraphs
    .into_iter()
    .filter(|paragraph| !paragraph.runs.is_empty())
    .collect()
}
