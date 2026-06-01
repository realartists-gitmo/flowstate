#[hotpath::measure_all]
impl RichTextEditor {
  pub fn find_text(&self, query: &str) -> Vec<Range<DocumentOffset>> {
    find_text_ranges(&self.document, query)
  }

  pub fn style_state(&self) -> RichTextEditorStyleState {
    if let Some(paragraph) = self.selected_table_cell_paragraph() {
      let mut semantic = SelectionStateBuilder::default();
      let mut underline = SelectionStateBuilder::default();
      let mut strikethrough = SelectionStateBuilder::default();
      let mut highlight = SelectionStateBuilder::default();
      let mut push_run_styles = |styles: RunStyles| {
        semantic.push(styles.semantic);
        underline.push(styles.direct_underline || styles.semantic == RunSemanticStyle::Underline);
        strikethrough.push(styles.strikethrough);
        highlight.push(styles.highlight);
      };
      if paragraph.paragraph.runs.is_empty() {
        push_run_styles(RunStyles::default());
      } else {
        for run in &paragraph.paragraph.runs {
          push_run_styles(run.styles);
        }
      }
      return RichTextEditorStyleState {
        paragraph_style: SelectionState::Uniform(paragraph.paragraph.style),
        semantic: semantic.finish(),
        underline: underline.finish(),
        strikethrough: strikethrough.finish(),
        highlight: highlight.finish(),
      };
    }
    let range = self.selection.normalized();
    let mut paragraph_style = SelectionStateBuilder::default();
    let mut semantic = SelectionStateBuilder::default();
    let mut underline = SelectionStateBuilder::default();
    let mut strikethrough = SelectionStateBuilder::default();
    let mut highlight = SelectionStateBuilder::default();

    if self.selection.is_caret() {
      if let Some(paragraph) = self.document.paragraphs.get(range.start.paragraph) {
        paragraph_style.push(paragraph.style);
      }
      let styles = self.styles_at_caret();
      semantic.push(styles.semantic);
      underline.push(styles.direct_underline || styles.semantic == RunSemanticStyle::Underline);
      strikethrough.push(styles.strikethrough);
      highlight.push(styles.highlight);
    } else {
      for paragraph_ix in range.start.paragraph..=range.end.paragraph {
        let Some(paragraph) = self.document.paragraphs.get(paragraph_ix) else {
          continue;
        };
        paragraph_style.push(paragraph.style);
        let start = if paragraph_ix == range.start.paragraph { range.start.byte } else { 0 };
        let end = if paragraph_ix == range.end.paragraph {
          range.end.byte
        } else {
          paragraph_text_len(paragraph)
        };
        let mut offset = 0;
        for run in &paragraph.runs {
          let run_start = offset;
          let run_end = offset + run.len;
          offset = run_end;
          if run_start < end && run_end > start {
            let styles = run.styles;
            semantic.push(styles.semantic);
            underline.push(styles.direct_underline || styles.semantic == RunSemanticStyle::Underline);
            strikethrough.push(styles.strikethrough);
            highlight.push(styles.highlight);
          }
        }
        if paragraph_style.is_mixed() && semantic.is_mixed() && underline.is_mixed() && strikethrough.is_mixed() && highlight.is_mixed() {
          break;
        }
      }
    }

    RichTextEditorStyleState {
      paragraph_style: paragraph_style.finish(),
      semantic: semantic.finish(),
      underline: underline.finish(),
      strikethrough: strikethrough.finish(),
      highlight: highlight.finish(),
    }
  }

  pub fn document_theme(&self) -> DocumentTheme {
    self.document.theme.clone()
  }

  pub fn has_unsaved_changes(&self) -> bool {
    self.edit_generation != self.saved_generation
  }

  pub fn edit_generation(&self) -> u64 {
    self.edit_generation
  }

  pub fn update_document_theme(&mut self, update: impl FnOnce(&mut DocumentTheme), cx: &mut Context<Self>) {
    update(&mut self.document.theme);
    self.invalidate_document_theme_layout(cx);
  }

  fn invalidate_document_theme_layout(&mut self, cx: &mut Context<Self>) {
    self.clear_layout_work_caches();
    self.paragraph_chunk_layout_cache = vec![None; self.document.paragraphs.len()];
    self.paragraph_height_cache = vec![None; self.document.paragraphs.len()];
    self.paragraph_height_cache_revision = self.paragraph_height_cache_revision.wrapping_add(1);
    self.item_sizes_cache = None;
    self.pending_item_sizes_patch_range = None;
    self.height_prefix_index = HeightPrefixIndex::default();
    cx.notify();
  }

  fn invalidate_document_layout_caches(&mut self) {
    self.clear_layout_work_caches();
    self.clear_all_layout_prep();
    self.paragraph_chunk_layout_cache = vec![None; self.document.paragraphs.len()];
    self.paragraph_height_cache = vec![None; self.document.paragraphs.len()];
    self.paragraph_height_cache_revision = self.paragraph_height_cache_revision.wrapping_add(1);
    self.item_sizes_cache = None;
    self.pending_item_sizes_patch_range = None;
    self.height_prefix_index = HeightPrefixIndex::default();
  }

  fn invalidate_stale_paragraph_layout_caches(&mut self) {
    let paragraph_count = self.document.paragraphs.len();
    let width = self.current_layout_width();
    self.clear_layout_work_caches();
    self.clear_all_layout_prep();
    self
      .paragraph_chunk_layout_cache
      .resize(paragraph_count, None);
    self.paragraph_height_cache.resize(paragraph_count, None);

    for paragraph_ix in 0..paragraph_count {
      let Some(paragraph) = self.document.paragraphs.get(paragraph_ix) else {
        self.paragraph_chunk_layout_cache[paragraph_ix] = None;
        self.paragraph_height_cache[paragraph_ix] = None;
        continue;
      };
      let key = paragraph_cache_key(&self.document, paragraph);
      let chunk_valid = self
        .valid_chunk_cache_entry(paragraph_ix, width)
        .is_some();
      if !chunk_valid {
        self.paragraph_chunk_layout_cache[paragraph_ix] = None;
      }

      let height_valid = self
        .paragraph_height_cache
        .get(paragraph_ix)
        .and_then(|entry| entry.as_ref())
        .is_some_and(|entry| {
          entry.key == key
            && entry.width == width
            && entry.invisibility_mode == self.invisibility_mode
            && entry.edit_generation == self.edit_generation
        });
      if !height_valid {
        self.paragraph_height_cache[paragraph_ix] = None;
      }
    }

    self.paragraph_height_cache_revision = self.paragraph_height_cache_revision.wrapping_add(1);
    self.item_sizes_cache = None;
    self.pending_item_sizes_patch_range = None;
    self.height_prefix_index = HeightPrefixIndex::default();
  }

  fn invalidate_paragraph_layout_cache_range(&mut self, range: Range<usize>) {
    let paragraph_count = self.document.paragraphs.len();
    let expanded_range = expand_paragraph_range(range, paragraph_count, 2);
    self.clear_layout_work_cache_range(expanded_range.clone());
    self.clear_layout_prep_range(expanded_range.clone());
    self
      .paragraph_chunk_layout_cache
      .resize(paragraph_count, None);
    self.paragraph_height_cache.resize(paragraph_count, None);

    for paragraph_ix in expanded_range.clone() {
      if let Some(cache) = self.paragraph_chunk_layout_cache.get_mut(paragraph_ix) {
        *cache = None;
      }
      if let Some(cache) = self.paragraph_height_cache.get_mut(paragraph_ix) {
        *cache = None;
      }
    }

    self.paragraph_height_cache_revision = self.paragraph_height_cache_revision.wrapping_add(1);
    self.pending_item_sizes_patch_range = Some(match self.pending_item_sizes_patch_range.take() {
      Some(previous) => previous.start.min(expanded_range.start)..previous.end.max(expanded_range.end),
      None => expanded_range,
    });
  }

  pub fn invisibility_mode(&self) -> bool {
    self.invisibility_mode
  }

  pub fn set_invisibility_mode(&mut self, enabled: bool, cx: &mut Context<Self>) {
    if self.invisibility_mode == enabled {
      return;
    }
    self.invisibility_mode = enabled;
    self.pending_layout_prep_task = None;
    self.pending_layout_prep_request = None;
    self.clear_layout_work_caches();
    self.paragraph_chunk_layout_cache = vec![None; self.document.paragraphs.len()];
    self.paragraph_height_cache = vec![None; self.document.paragraphs.len()];
    self.paragraph_height_cache_revision = self.paragraph_height_cache_revision.wrapping_add(1);
    self.item_sizes_cache = None;
    self.pending_item_sizes_patch_range = None;
    self.height_prefix_index = HeightPrefixIndex::default();
    self.pending_scroll_head_after_layout = true;
    cx.notify();
  }

  pub fn toggle_invisibility_mode(&mut self, cx: &mut Context<Self>) {
    self.set_invisibility_mode(!self.invisibility_mode, cx);
  }

  pub fn save(&mut self, cx: &mut Context<Self>) -> Task<io::Result<()>> {
    if self.disposed {
      return cx
        .background_executor()
        .spawn(async { Err(io::Error::new(io::ErrorKind::NotFound, "editor is closed")) });
    }
    let Some(path) = self.document_path.clone() else {
      return cx
        .background_executor()
        .spawn(async { Err(io::Error::new(io::ErrorKind::InvalidInput, "choose a save location before saving")) });
    };
    self.save_to_path(path, cx)
  }

  pub fn save_as(&mut self, path: PathBuf, cx: &mut Context<Self>) -> Task<io::Result<()>> {
    if self.disposed {
      return cx
        .background_executor()
        .spawn(async { Err(io::Error::new(io::ErrorKind::NotFound, "editor is closed")) });
    }
    self.document_path = Some(path.clone());
    self.recovery_path = Some(recovery_path_for_document(&path));
    self.save_to_path(path, cx)
  }

  fn save_to_path(&mut self, path: PathBuf, cx: &mut Context<Self>) -> Task<io::Result<()>> {
    if self.disposed {
      return cx
        .background_executor()
        .spawn(async { Err(io::Error::new(io::ErrorKind::NotFound, "editor is closed")) });
    }
    let generation = self.edit_generation;
    let document = self.document.clone();
    let recovery_path = self.recovery_path.clone();
    self.save_status = SaveStatus::Saving;
    cx.notify();
    cx.spawn(async move |editor, cx| {
      let write_result = cx
        .background_executor()
        .spawn(async move {
          let document = detach_document_for_background_write(&document);
          let result = write_db8(&path, &document);
          if result.is_ok()
            && let Some(recovery_path) = recovery_path
          {
            let _ = fs::remove_file(recovery_path);
          }
          result
        })
        .await;
      match write_result {
        Ok(()) => {
          let _ = editor.update(cx, |editor, cx| {
            editor.saved_generation = editor.saved_generation.max(generation);
            editor.refresh_save_status();
            cx.notify();
          });
          Ok(())
        },
        Err(error) => {
          let message = error.to_string();
          let _ = editor.update(cx, |editor, cx| {
            if generation >= editor.saved_generation {
              editor.save_status = SaveStatus::SaveFailed(message);
            }
            cx.notify();
          });
          Err(error)
        },
      }
    })
  }

  pub fn discard_recovery_file(&mut self, cx: &mut Context<Self>) {
    if let Some(path) = self.recovery_path.clone() {
      cx.background_executor().spawn(async move {
        let _ = fs::remove_file(path);
      })
      .detach();
    }
  }

}
