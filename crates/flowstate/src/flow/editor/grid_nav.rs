//! The grid keyboard grammar (excel flow spec D5): live cells wrapped in
//! Excel navigation — a (row, column) cursor that walks real rows and the
//! ghost run, verbs that materialize rows on demand (one undo group), and
//! the refusal voice (silent refusal is a defect).

use flowstate_flow::{CellId, CellSeed, FlowIntent, RowId, Sheet};
use gpui::Context;

use super::FlowEditor;
use super::grid_layout::GHOST_ROWS;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridDirection {
  Up,
  Down,
  Left,
  Right,
}

/// Row-relative placement for the "Above / Below" verbs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelativePosition {
  Before,
  After,
}

impl FlowEditor {
  /// The active sheet's projection, or `None` with no board.
  pub(super) fn active_sheet_ref(&self) -> Option<&Sheet> {
    let sheet_id = self.active_sheet?;
    self.board.sheets.iter().find(|sheet| sheet.id == sheet_id)
  }

  /// §3.1 F3: refusals SPEAK. Post the reason; the render layer paints it as
  /// a transient toast (and shakes the named cell when one is given).
  pub(super) fn refuse(&mut self, reason: impl Into<String>, cell: Option<CellId>, cx: &mut Context<Self>) {
    let message = reason.into();
    self.log_intent(format!("refused: {message}"));
    self.refusal = Some(super::RefusalNotice {
      message,
      cell,
      at: std::time::Instant::now(),
    });
    cx.notify();
  }

  /// Age out the refusal toast (~2.4s) — called from render.
  pub(super) fn refusal_toast(&mut self) -> Option<(String, Option<CellId>, f32)> {
    let notice = self.refusal.as_ref()?;
    let age = notice.at.elapsed().as_secs_f32();
    if age > 2.4 {
      self.refusal = None;
      return None;
    }
    Some((notice.message.clone(), notice.cell, age))
  }

  /// Place the grid cursor (clamped to the extended grid) and derive the
  /// active cell from the slot under it. Landing on a cell activates it.
  pub fn set_cursor(&mut self, row_ix: usize, column_ix: usize, cx: &mut Context<Self>) {
    let Some(sheet) = self.active_sheet_ref() else {
      return;
    };
    let columns = sheet.columns.len();
    if columns == 0 {
      return;
    }
    let max_row = sheet.rows.len() + GHOST_ROWS - 1;
    let row_ix = row_ix.min(max_row);
    let column_ix = column_ix.min(columns - 1);
    let occupant = sheet.slot(row_ix, column_ix).map(|cell| cell.id);
    self.cursor = Some((row_ix, column_ix));
    match occupant {
      Some(cell) => self.activate_cell(cell, cx),
      None => {
        if self.active_cell.take().is_some() {
          cx.emit(super::FlowEditorEvent::ActiveCellChanged(None));
        }
        cx.notify();
      },
    }
    // No auto-scroll here: a mouse click must not snap the viewport. Keyboard
    // navigation calls `scroll_cursor_into_view` itself.
  }

  pub fn cursor(&self) -> Option<(usize, usize)> {
    self.cursor
  }

  /// R4 — arrows/Tab/Enter walk the grid Excel-style, through empty slots
  /// and into the ghost run.
  pub fn navigate(&mut self, direction: GridDirection, cx: &mut Context<Self>) {
    let Some(sheet) = self.active_sheet_ref() else {
      return;
    };
    if sheet.columns.is_empty() {
      return;
    }
    let (row_ix, column_ix) = self.cursor.unwrap_or((0, 0));
    let (row_ix, column_ix) = match direction {
      GridDirection::Up => (row_ix.saturating_sub(1), column_ix),
      GridDirection::Down => (row_ix + 1, column_ix),
      GridDirection::Left => (row_ix, column_ix.saturating_sub(1)),
      GridDirection::Right => (row_ix, column_ix + 1),
    };
    self.set_cursor(row_ix, column_ix, cx);
    // Keyboard nav keeps the cursor on-screen (mouse selection does not).
    self.scroll_cursor_into_view();
  }

  /// R9 — move the active cell one slot: two LWW register writes (D1), with
  /// ghost targets materialized in the same undo group. Occupied targets and
  /// edges refuse WITH WORDS.
  pub fn move_active_cell(&mut self, direction: GridDirection, cx: &mut Context<Self>) {
    let Some((sheet_id, active)) = self.active_sheet.zip(self.active_cell) else {
      return;
    };
    let Some(sheet) = self.active_sheet_ref() else {
      return;
    };
    let Some((row_ix, column_ix)) = sheet.cell_position(active) else {
      return;
    };
    let (target_row, target_column) = match direction {
      GridDirection::Up => {
        let Some(row) = row_ix.checked_sub(1) else {
          self.refuse("already in the first row", Some(active), cx);
          return;
        };
        (row, column_ix)
      },
      GridDirection::Down => (row_ix + 1, column_ix),
      GridDirection::Left => {
        let Some(column) = column_ix.checked_sub(1) else {
          self.refuse("already in the first column", Some(active), cx);
          return;
        };
        (row_ix, column)
      },
      GridDirection::Right => {
        if column_ix + 1 >= sheet.columns.len() {
          self.refuse("already in the last column", Some(active), cx);
          return;
        }
        (row_ix, column_ix + 1)
      },
    };
    self.move_cell_to_slot(sheet_id, active, target_row, target_column, cx);
  }

  /// The one cell-move path (keyboard and drag both land here): resolve the
  /// (possibly ghost) target slot, SWAP with an occupant (lossless), else
  /// materialize ghost rows + set the address — all as one undo group.
  pub(super) fn move_cell_to_slot(&mut self, sheet_id: flowstate_flow::SheetId, cell_id: CellId, row_ix: usize, column_ix: usize, cx: &mut Context<Self>) {
    let Some(sheet) = self.active_sheet_ref() else {
      return;
    };
    let Some(column_id) = sheet.columns.get(column_ix).map(|column| column.id) else {
      return;
    };
    if let Some(occupant) = sheet.slot(row_ix, column_ix) {
      if occupant.id == cell_id {
        return;
      }
      // Drop onto an occupied slot swaps: the occupant takes the dragged
      // cell's vacated address. One atomic intent (SwapCells) so the pair
      // never trips the empty-slot guard.
      let occupant_id = occupant.id;
      if self
        .apply_intent(
          &FlowIntent::SwapCells {
            sheet_id,
            a: cell_id,
            b: occupant_id,
          },
          cx,
        )
        .is_ok()
      {
        self.changed(Some(cell_id), cx);
        self.cursor = Some((row_ix, column_ix));
        self.scroll_cursor_into_view();
      }
      return;
    }
    let needs_rows = (row_ix + 1).saturating_sub(sheet.rows.len());
    let existing_row = sheet.rows.get(row_ix).map(|row| row.id);
    let grouped = needs_rows > 0;
    if grouped {
      let _ = self.handle.undo_group_start();
    }
    let row_id = match existing_row {
      Some(row_id) => Some(row_id),
      None => self.materialize_rows(sheet_id, needs_rows, cx),
    };
    let moved = row_id.is_some_and(|row_id| {
      self
        .apply_intent(
          &FlowIntent::SetCellAddress {
            sheet_id,
            cell_id,
            row_id,
            column_id,
          },
          cx,
        )
        .is_ok()
    });
    if grouped {
      let _ = self.handle.undo_group_end();
    }
    if moved {
      self.changed(Some(cell_id), cx);
      self.cursor = Some((row_ix, column_ix));
      self.scroll_cursor_into_view();
    }
  }

  /// Drag a multi-selection: translate the whole set by the delta the dragged
  /// cell travels, as ONE atomic block-swap. The block moves +Δ into its new
  /// footprint; any cards it lands on that AREN'T in the set are collected and
  /// slid into the slots the block vacated (old footprint − new footprint, in
  /// reading order). Lossless — the vacated slots always outnumber the
  /// displaced cards. Off-sheet destinations refuse with words.
  pub(crate) fn move_selection_block(
    &mut self,
    sheet_id: flowstate_flow::SheetId,
    dragged: CellId,
    target_row: usize,
    target_column: usize,
    cx: &mut Context<Self>,
  ) {
    let Some(sheet) = self.active_sheet_ref() else { return };
    let columns = sheet.columns.len();
    let Some((origin_row, origin_column)) = sheet.cell_position(dragged) else { return };
    let d_row = target_row as isize - origin_row as isize;
    let d_column = target_column as isize - origin_column as isize;
    if d_row == 0 && d_column == 0 {
      return;
    }
    let members = self.operation_set(sheet_id);
    let moving: std::collections::HashSet<CellId> = members.iter().copied().collect();

    // The block's own moves, plus its source and destination footprints.
    let mut plan: Vec<(CellId, usize, usize)> = Vec::with_capacity(members.len());
    let mut sources: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
    let mut dests: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
    let mut displaced: Vec<(CellId, (usize, usize))> = Vec::new();
    let mut max_row = 0usize;
    for &id in &members {
      let Some((r, c)) = sheet.cell_position(id) else { continue };
      let nr = r as isize + d_row;
      let nc = c as isize + d_column;
      if nr < 0 || nc < 0 || nc as usize >= columns {
        self.refuse("that move runs off the sheet — the set stays put", Some(dragged), cx);
        return;
      }
      let (nr, nc) = (nr as usize, nc as usize);
      sources.insert((r, c));
      dests.insert((nr, nc));
      // A card outside the set sitting on our destination gets swapped back.
      if let Some(occupant) = sheet.slot(nr, nc)
        && !moving.contains(&occupant.id)
      {
        displaced.push((occupant.id, (nr, nc)));
      }
      plan.push((id, nr, nc));
      max_row = max_row.max(nr);
    }
    if plan.is_empty() {
      return;
    }
    // Vacated = old footprint the block does not re-occupy. Map the displaced
    // cards into it in reading order (leftover vacated slots just stay empty).
    let mut vacated: Vec<(usize, usize)> = sources.iter().copied().filter(|slot| !dests.contains(slot)).collect();
    vacated.sort_unstable();
    displaced.sort_unstable_by_key(|&(_, slot)| slot);
    for (index, (cell_id, _)) in displaced.into_iter().enumerate() {
      let Some(&(vr, vc)) = vacated.get(index) else { break };
      plan.push((cell_id, vr, vc));
    }

    let needs_rows = (max_row + 1).saturating_sub(sheet.rows.len());
    let _ = self.handle.undo_group_start();
    if needs_rows > 0 {
      self.materialize_rows(sheet_id, needs_rows, cx);
    }
    // Re-resolve durable ids after any ghost materialization, then land the
    // whole permutation in one atomic intent.
    let placements: Option<Vec<(CellId, RowId, flowstate_flow::ColumnId)>> = self.active_sheet_ref().map(|sheet| {
      plan
        .iter()
        .filter_map(|&(id, nr, nc)| Some((id, sheet.rows.get(nr)?.id, sheet.columns.get(nc)?.id)))
        .collect()
    });
    let mut moved = false;
    if let Some(placements) = placements
      && placements.len() == plan.len()
    {
      moved = self
        .apply_intent(&FlowIntent::SetCellAddresses { sheet_id, placements }, cx)
        .is_ok();
    }
    let _ = self.handle.undo_group_end();
    if moved {
      self.changed(Some(dragged), cx);
      self.cursor = Some((target_row, target_column));
      self.scroll_cursor_into_view();
    }
  }

  /// Append `count` fresh rows; returns the LAST minted row id.
  pub(super) fn materialize_rows(&mut self, sheet_id: flowstate_flow::SheetId, count: usize, cx: &mut Context<Self>) -> Option<RowId> {
    if count == 0 {
      return None;
    }
    let row_ids: Vec<RowId> = (0..count).map(|_| uuid::Uuid::new_v4()).collect();
    let last = *row_ids.last()?;
    self
      .apply_intent(
        &FlowIntent::InsertRows {
          sheet_id,
          before: None,
          row_ids,
        },
        cx,
      )
      .ok()?;
    Some(last)
  }

  /// Mint a cell at a (possibly ghost) slot — the ghost-materialization law:
  /// rows + cell land as ONE undo group. On success the cell activates with
  /// an editor attached.
  pub(super) fn add_cell_at_slot(&mut self, row_ix: usize, column_ix: usize, seed: CellSeed, cx: &mut Context<Self>) -> Option<CellId> {
    let sheet_id = self.active_sheet?;
    let sheet = self.active_sheet_ref()?;
    let column_id = sheet.columns.get(column_ix).map(|column| column.id)?;
    if let Some(occupant) = sheet.slot(row_ix, column_ix) {
      let occupant = occupant.id;
      self.refuse("that slot already holds a card", Some(occupant), cx);
      return None;
    }
    let needs_rows = (row_ix + 1).saturating_sub(sheet.rows.len());
    let existing_row = sheet.rows.get(row_ix).map(|row| row.id);
    let grouped = needs_rows > 0;
    if grouped {
      let _ = self.handle.undo_group_start();
    }
    let row_id = match existing_row {
      Some(row_id) => Some(row_id),
      None => self.materialize_rows(sheet_id, needs_rows, cx),
    };
    let cell_id = uuid::Uuid::new_v4();
    let added = row_id.is_some_and(|row_id| {
      self
        .apply_intent(
          &FlowIntent::AddCell {
            sheet_id,
            cell_id,
            row_id,
            column_id,
            seed,
          },
          cx,
        )
        .is_ok()
    });
    if grouped {
      let _ = self.handle.undo_group_end();
    }
    if !added {
      return None;
    }
    self.cursor = Some((row_ix, column_ix));
    self.ensure_cell_editor(cell_id, cx);
    self.changed(Some(cell_id), cx);
    self.scroll_cursor_into_view();
    Some(cell_id)
  }

  /// Typing on an empty slot IS creation (Excel muscle memory): the typed
  /// text seeds the new cell so the first keystroke is never lost.
  pub fn type_into_cursor(&mut self, text: &str, cx: &mut Context<Self>) -> Option<CellId> {
    let (row_ix, column_ix) = self.cursor?;
    let seed = CellSeed::Paragraphs(vec![flowstate_document::InputParagraph {
      style: flowstate_document::PARAGRAPH_TAG,
      runs: vec![flowstate_document::InputRun {
        text: text.to_string(),
        styles: flowstate_document::RunStyles::default(),
      }],
    }]);
    self.add_cell_at_slot(row_ix, column_ix, seed, cx)
  }

  /// "Above / Below": a fresh ROW next to the cursor's row, with a new cell
  /// in the cursor's column (one undo group).
  pub fn add_sibling(&mut self, position: RelativePosition, cx: &mut Context<Self>) {
    let Some(sheet_id) = self.active_sheet else {
      return;
    };
    let Some(sheet) = self.active_sheet_ref() else {
      return;
    };
    let (row_ix, column_ix) = self.cursor.unwrap_or((sheet.rows.len(), 0));
    // Anchoring: Before = the cursor row; After = the row below it.
    let (anchor, new_row_ix) = match position {
      RelativePosition::Before => (sheet.rows.get(row_ix).map(|row| row.id), row_ix.min(sheet.rows.len())),
      RelativePosition::After => (sheet.rows.get(row_ix + 1).map(|row| row.id), (row_ix + 1).min(sheet.rows.len())),
    };
    if row_ix >= sheet.rows.len() {
      // Cursor already in the ghost run: creation IS the verb.
      self.add_cell_at_slot(row_ix, column_ix, CellSeed::Empty, cx);
      return;
    }
    let row_id = uuid::Uuid::new_v4();
    let column_id = sheet.columns.get(column_ix).map(|column| column.id);
    let Some(column_id) = column_id else { return };
    let _ = self.handle.undo_group_start();
    let inserted = self
      .apply_intent(
        &FlowIntent::InsertRows {
          sheet_id,
          before: anchor,
          row_ids: vec![row_id],
        },
        cx,
      )
      .is_ok();
    let cell_id = uuid::Uuid::new_v4();
    let added = inserted
      && self
        .apply_intent(
          &FlowIntent::AddCell {
            sheet_id,
            cell_id,
            row_id,
            column_id,
            seed: CellSeed::Empty,
          },
          cx,
        )
        .is_ok();
    let _ = self.handle.undo_group_end();
    if added {
      self.cursor = Some((new_row_ix, column_ix));
      self.ensure_cell_editor(cell_id, cx);
      self.changed(Some(cell_id), cx);
      self.scroll_cursor_into_view();
    }
  }

  /// "Response": the answer lands one column RIGHT, same row — the excel
  /// flow's who-answers-what is horizontal adjacency, owned by the user.
  pub fn add_response(&mut self, cx: &mut Context<Self>) {
    let Some(sheet) = self.active_sheet_ref() else {
      return;
    };
    let Some((row_ix, column_ix)) = self
      .cursor
      .or_else(|| self.active_cell.and_then(|cell| sheet.cell_position(cell)))
    else {
      return;
    };
    if column_ix + 1 >= sheet.columns.len() {
      self.refuse("already in the last column — nowhere to answer to", self.active_cell, cx);
      return;
    }
    if let Some(occupant) = sheet.slot(row_ix, column_ix + 1) {
      // The slot already answers: go there instead of refusing outright.
      let occupant = occupant.id;
      self.set_cursor(row_ix, column_ix + 1, cx);
      self.log_intent(format!("response slot occupied → activated {occupant}"));
      return;
    }
    self.add_cell_at_slot(row_ix, column_ix + 1, CellSeed::Empty, cx);
  }

  /// "Argument": a fresh row at the bottom, first column.
  pub fn add_first_argument(&mut self, cx: &mut Context<Self>) {
    let Some(sheet) = self.active_sheet_ref() else {
      return;
    };
    let row_ix = sheet.rows.len();
    self.add_cell_at_slot(row_ix, 0, CellSeed::Empty, cx);
  }

  /// Ctrl/Cmd-Enter: a fresh row at the bottom in the ACTIVE column.
  pub fn add_new_family(&mut self, cx: &mut Context<Self>) {
    let Some(sheet) = self.active_sheet_ref() else {
      return;
    };
    let column_ix = self.cursor.map(|(_, column)| column).unwrap_or(0);
    let row_ix = sheet.rows.len();
    self.add_cell_at_slot(row_ix, column_ix, CellSeed::Empty, cx);
  }

  /// Column-header "+": the first empty slot from the top of that column
  /// (fills the gap the way paper flowing does), else a fresh bottom row.
  pub fn add_cell_in_column(&mut self, column_ix: usize, cx: &mut Context<Self>) {
    let Some(sheet) = self.active_sheet_ref() else {
      return;
    };
    let row_ix = (0..sheet.rows.len())
      .find(|&row_ix| sheet.slot(row_ix, column_ix).is_none())
      .unwrap_or(sheet.rows.len());
    self.add_cell_at_slot(row_ix, column_ix, CellSeed::Empty, cx);
  }
}
