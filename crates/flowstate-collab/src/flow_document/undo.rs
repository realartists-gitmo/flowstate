use std::sync::{Arc, Mutex};

use loro::{LoroDoc, LoroValue, UndoItemMeta, UndoManager};

use super::AnchoredSelection;
use crate::{CollabError, CollabResult};

const TYPING_MERGE_INTERVAL_MS: i64 = 750;

/// Local Loro undo lineage with explicit grouping and anchored selection
/// metadata. Remote, migration, recovery, and host-authored changes are never
/// added to this lineage.
#[derive(Debug)]
pub struct FlowUndoManager {
  inner: UndoManager,
  selection_for_next_item: Arc<Mutex<Option<AnchoredSelection>>>,
  popped_selection: Arc<Mutex<Option<AnchoredSelection>>>,
}

impl FlowUndoManager {
  pub(super) fn new(doc: &LoroDoc) -> Self {
    let selection_for_next_item = Arc::new(Mutex::new(None::<AnchoredSelection>));
    let popped_selection = Arc::new(Mutex::new(None::<AnchoredSelection>));
    let push_selection = selection_for_next_item.clone();
    let pop_selection = popped_selection.clone();
    let mut inner = UndoManager::new(doc);
    inner.set_merge_interval(TYPING_MERGE_INTERVAL_MS);
    inner.add_exclude_origin_prefix("flowstate:remote");
    inner.add_exclude_origin_prefix("flowstate:migration");
    inner.add_exclude_origin_prefix("flowstate:recovery");
    inner.add_exclude_origin_prefix("flowstate:host");
    inner.set_on_push(Some(Box::new(move |_, _, _| {
      let mut metadata = UndoItemMeta::new();
      if let Some(selection) = push_selection.lock().ok().and_then(|selection| selection.clone())
        && let Ok(bytes) = postcard::to_stdvec(&selection)
      {
        metadata.set_value(LoroValue::from(bytes));
        metadata.add_cursor(&selection.anchor.cursor);
        metadata.add_cursor(&selection.head.cursor);
      }
      metadata
    })));
    inner.set_on_pop(Some(Box::new(move |_, _, metadata| {
      let mut selection: Option<AnchoredSelection> = match metadata.value {
        LoroValue::Binary(bytes) => postcard::from_bytes(&bytes).ok(),
        _ => None,
      };
      if let Some(selection) = &mut selection
        && let [anchor, head] = metadata.cursors.as_slice()
      {
        selection.anchor.cursor = anchor.cursor.clone();
        selection.head.cursor = head.cursor.clone();
      }
      if let Ok(mut popped) = pop_selection.lock() {
        *popped = selection;
      }
    })));
    Self {
      inner,
      selection_for_next_item,
      popped_selection,
    }
  }

  pub fn set_selection_for_next_item(&self, selection: Option<AnchoredSelection>) -> CollabResult<()> {
    *self
      .selection_for_next_item
      .lock()
      .map_err(|_| CollabError::InvalidSchema("vNext undo selection lock poisoned"))? = selection;
    Ok(())
  }

  pub fn take_popped_selection(&self) -> CollabResult<Option<AnchoredSelection>> {
    Ok(
      self
        .popped_selection
        .lock()
        .map_err(|_| CollabError::InvalidSchema("vNext popped undo selection lock poisoned"))?
        .take(),
    )
  }

  pub fn begin_isolated_group(&mut self) -> CollabResult<()> {
    self.inner.set_merge_interval(0);
    if let Err(error) = self.inner.group_start() {
      self.inner.set_merge_interval(TYPING_MERGE_INTERVAL_MS);
      return Err(super::loro_error(error));
    }
    Ok(())
  }

  pub fn end_isolated_group(&mut self) {
    self.inner.group_end();
    self.inner.set_merge_interval(TYPING_MERGE_INTERVAL_MS);
  }

  pub(super) fn undo(&mut self) -> loro::LoroResult<bool> {
    self.inner.undo()
  }

  pub(super) fn redo(&mut self) -> loro::LoroResult<bool> {
    self.inner.redo()
  }
}
