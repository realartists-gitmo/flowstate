//! The grid keyboard grammar (excel flow spec D5): live cells wrapped in
//! Excel navigation — a (row, column) cursor that walks real rows and the
//! ghost run, verbs that materialize rows on demand (one undo group), and
//! the refusal voice (silent refusal is a defect).

use flowstate_flow::{CellId, CellSeed, FlowIntent, RowId, Sheet};
use gpui::Context;
use gpui_component::PixelsExt as _;

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
      toasted: false,
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
    // A plain cursor move COLLAPSES any multi-cell selection and re-anchors
    // here (Excel). Without this, `selected_cells` outlives the move, and a
    // following Delete/Cut/Copy (which read `operation_set`) or Shift+Arrow
    // would silently act on the stale rectangle the cursor already left.
    self.selected_cells.clear();
    self.clear_selection_shape();
    self.selection_anchor = Some((row_ix, column_ix));
    match occupant {
      Some(cell) => self.activate_cell(cell, cx),
      None => {
        if self.active_cell.take().is_some() {
          cx.emit(super::FlowEditorEvent::ActiveCellChanged(None));
        }
        cx.notify();
      },
    }
    // G2: peers track the SLOT, not just cell activations.
    cx.emit(super::FlowEditorEvent::PresenceShifted);
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
      self.begin_bulk();
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
      self.end_bulk("edit", cx);
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
    self.begin_bulk();
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
    self.end_bulk("edit", cx);
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
      self.begin_bulk();
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
      self.end_bulk("edit", cx);
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
    self.begin_bulk();
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
    self.end_bulk("edit", cx);
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

  /// The extended grid bounds: `(max_row_index, column_count)`. `max_row`
  /// includes the ghost run, matching `set_cursor`'s clamp.
  fn grid_bounds(&self) -> Option<(usize, usize)> {
    let sheet = self.active_sheet_ref()?;
    let columns = sheet.columns.len();
    if columns == 0 {
      return None;
    }
    Some((sheet.rows.len() + GHOST_ROWS - 1, columns))
  }

  /// Tab (or Shift+Tab): move one column and remember where the run began, so
  /// a following Enter can return to it. Used by both board and edit modes.
  pub fn tab_navigate(&mut self, reverse: bool, cx: &mut Context<Self>) {
    if self.tab_anchor_column.is_none() {
      self.tab_anchor_column = self.cursor.map(|(_, column)| column);
    }
    let last_column = match self.active_sheet_ref() {
      Some(sheet) if !sheet.columns.is_empty() => sheet.columns.len() - 1,
      _ => {
        self.navigate(if reverse { GridDirection::Left } else { GridDirection::Right }, cx);
        return;
      },
    };
    let Some((row, column)) = self.cursor else {
      self.navigate(if reverse { GridDirection::Left } else { GridDirection::Right }, cx);
      return;
    };
    // Excel: Tab past the last column WRAPS to the next row at the Tab run's
    // start column; Shift+Tab past the first column wraps up to the last
    // column — never a silent dead-end at the row's edge.
    if !reverse && column >= last_column {
      let anchor = self.tab_anchor_column.unwrap_or(0).min(last_column);
      self.set_cursor(row + 1, anchor, cx);
      self.scroll_cursor_into_view();
      return;
    }
    if reverse && column == 0 {
      match row.checked_sub(1) {
        Some(prev) => {
          self.set_cursor(prev, last_column, cx);
          self.scroll_cursor_into_view();
        },
        None => self.refuse("already at the first cell", self.active_cell, cx),
      }
      return;
    }
    self.navigate(if reverse { GridDirection::Left } else { GridDirection::Right }, cx);
  }

  /// Enter (or Shift+Enter): move one row. After a Tab-run, plain Enter drops a
  /// row and returns to the run's start column (Excel); otherwise it's a plain
  /// vertical move. Any move consumes the tab anchor.
  pub fn enter_navigate(&mut self, up: bool, cx: &mut Context<Self>) {
    match self.tab_anchor_column.take() {
      Some(anchor_column) if !up => {
        let row = self.cursor.map(|(row, _)| row).unwrap_or(0);
        self.set_cursor(row + 1, anchor_column, cx);
        self.scroll_cursor_into_view();
      },
      _ => self.navigate(if up { GridDirection::Up } else { GridDirection::Down }, cx),
    }
  }

  /// Excel: when a multi-cell selection is active, Enter/Tab CYCLE the active
  /// cell within the selection (reading order, wrapping) instead of navigating
  /// the whole grid. Returns `false` (do a normal move) when ≤1 cell is
  /// selected. `forward` = Enter/Tab; `!forward` = Shift+Enter/Shift+Tab.
  pub fn cycle_within_selection(&mut self, forward: bool, cx: &mut Context<Self>) -> bool {
    if self.selected_cells.len() <= 1 {
      return false;
    }
    let Some(sheet) = self.active_sheet_ref() else {
      return false;
    };
    let mut positions: Vec<(usize, usize)> = self
      .selected_cells
      .iter()
      .filter_map(|id| sheet.cell_position(*id))
      .collect();
    positions.sort_unstable();
    if positions.is_empty() {
      return false;
    }
    let here = self.cursor.and_then(|cursor| positions.iter().position(|&slot| slot == cursor));
    let next = match here {
      Some(index) if forward => (index + 1) % positions.len(),
      Some(index) => (index + positions.len() - 1) % positions.len(),
      None => 0,
    };
    let (row, column) = positions[next];
    let cell = sheet.slot(row, column).map(|cell| cell.id);
    self.cursor = Some((row, column));
    if let Some(cell) = cell {
      self.activate_cell(cell, cx);
    }
    self.scroll_cursor_into_view();
    cx.notify();
    true
  }

  /// Shift+Arrow: grow/shrink the selection rectangle from the anchor (set to
  /// the current cursor on the first extend). The moving end becomes the cursor.
  pub fn extend_selection(&mut self, direction: GridDirection, cx: &mut Context<Self>) {
    let Some((max_row, columns)) = self.grid_bounds() else {
      return;
    };
    let (row, col) = self.cursor.unwrap_or((0, 0));
    if self.selection_anchor.is_none() {
      self.selection_anchor = Some((row, col));
    }
    let (nr, nc) = match direction {
      GridDirection::Up => (row.saturating_sub(1), col),
      GridDirection::Down => ((row + 1).min(max_row), col),
      GridDirection::Left => (row, col.saturating_sub(1)),
      GridDirection::Right => (row, (col + 1).min(columns - 1)),
    };
    self.select_cell_range(nr, nc, cx);
    self.scroll_cursor_into_view();
  }

  /// Ctrl+Arrow target: jump to the edge of the current data block (Excel
  /// rules). `None` = can't move (already at the sheet edge in that direction).
  fn edge_target(&self, direction: GridDirection) -> Option<(usize, usize)> {
    let sheet = self.active_sheet_ref()?;
    let columns = sheet.columns.len();
    if columns == 0 {
      return None;
    }
    let max_row = sheet.rows.len() + GHOST_ROWS - 1;
    let (row, col) = self.cursor?;
    let (dr, dc): (isize, isize) = match direction {
      GridDirection::Up => (-1, 0),
      GridDirection::Down => (1, 0),
      GridDirection::Left => (0, -1),
      GridDirection::Right => (0, 1),
    };
    let step = |(r, c): (usize, usize)| -> Option<(usize, usize)> {
      let nr = r as isize + dr;
      let nc = c as isize + dc;
      if nr < 0 || nc < 0 {
        return None;
      }
      let (nr, nc) = (nr as usize, nc as usize);
      if nr > max_row || nc >= columns {
        return None;
      }
      Some((nr, nc))
    };
    let occ = |(r, c): (usize, usize)| sheet.slot(r, c).is_some();
    let first = step((row, col))?;
    let mut cur = first;
    if !occ((row, col)) {
      // From a blank cell: advance to the first cell with data (or the edge).
      while !occ(cur) {
        match step(cur) {
          Some(next) => cur = next,
          None => break,
        }
      }
    } else if occ(first) {
      // In data, next has data: ride to the last cell before a blank/edge.
      while let Some(next) = step(cur) {
        if occ(next) {
          cur = next;
        } else {
          break;
        }
      }
    } else {
      // In data, next is blank: cross the gap to the next cell with data.
      loop {
        match step(cur) {
          Some(next) if occ(next) => {
            cur = next;
            break;
          },
          Some(next) => cur = next,
          None => break,
        }
      }
    }
    Some(cur)
  }

  /// Ctrl+Arrow: jump to the data-block edge. `extend` = Ctrl+Shift+Arrow.
  pub fn jump_to_edge(&mut self, direction: GridDirection, extend: bool, cx: &mut Context<Self>) {
    let Some((row, col)) = self.edge_target(direction) else {
      // A5: a dead keypress must speak.
      self.refuse("place the cursor on the grid first", None, cx);
      return;
    };
    if extend {
      if self.selection_anchor.is_none() {
        self.selection_anchor = self.cursor;
      }
      self.select_cell_range(row, col, cx);
    } else {
      self.set_cursor(row, col, cx);
    }
    self.scroll_cursor_into_view();
  }

  /// The bottom-right corner of the used range (highest row/column holding a
  /// cell), `(0, 0)` on an empty sheet.
  fn used_extent(&self) -> (usize, usize) {
    let Some(sheet) = self.active_sheet_ref() else {
      return (0, 0);
    };
    let mut max_row = 0;
    let mut max_col = 0;
    for (row_ix, row) in sheet.rows.iter().enumerate() {
      for (column_ix, slot) in row.cells.iter().enumerate() {
        if slot.is_some() {
          max_row = max_row.max(row_ix);
          max_col = max_col.max(column_ix);
        }
      }
    }
    (max_row, max_col)
  }

  /// Home / End / Ctrl+Home / Ctrl+End.
  pub fn cursor_to_extreme(&mut self, key: &str, ctrl: bool, cx: &mut Context<Self>) {
    let Some((row, _col)) = self.cursor else {
      // A5: a dead keypress must speak.
      self.refuse("place the cursor on the grid first", None, cx);
      return;
    };
    let (r, c) = match (key, ctrl) {
      ("home", true) => (0, 0),
      ("end", true) => self.used_extent(),
      ("home", false) => (row, 0),
      ("end", false) => {
        let sheet = match self.active_sheet_ref() {
          Some(sheet) => sheet,
          None => return,
        };
        let last = (0..sheet.columns.len()).rev().find(|&c| sheet.slot(row, c).is_some());
        (row, last.unwrap_or(sheet.columns.len().saturating_sub(1)))
      },
      _ => return,
    };
    self.set_cursor(r, c, cx);
    self.scroll_cursor_into_view();
  }

  /// PageUp / PageDown: move the cursor by one viewport of rows.
  pub fn page(&mut self, down: bool, cx: &mut Context<Self>) {
    let Some((max_row, _)) = self.grid_bounds() else { return };
    let step = self.page_rows().max(1);
    let (row, col) = self.cursor.unwrap_or((0, 0));
    let target = if down { (row + step).min(max_row) } else { row.saturating_sub(step) };
    self.set_cursor(target, col, cx);
    self.scroll_cursor_into_view();
  }

  /// Rows that fit in the current viewport (for paging), floored to ≥1.
  fn page_rows(&self) -> usize {
    let Some((layout, _)) = self.active_layout() else { return 10 };
    let rows = layout.total_rows().max(1);
    let avg = (layout.total_height() / rows as f32).max(1.0);
    let viewport = self.board_scroll.bounds().size.height.as_f32().max(1.0) / self.board_zoom.max(0.01);
    ((viewport / avg).floor() as usize).max(1)
  }

  /// Ctrl+Space (and the column-header click): select every cell in a column.
  pub fn select_column(&mut self, column_ix: usize, cx: &mut Context<Self>) {
    let Some(sheet) = self.active_sheet_ref() else { return };
    let rows = sheet.rows.len();
    let set: std::collections::HashSet<CellId> = sheet
      .rows
      .iter()
      .filter_map(|row| row.cells.get(column_ix).and_then(|slot| slot.as_ref()).map(|cell| cell.id))
      .collect();
    self.selected_cells = set;
    self.selection_rect = (rows > 0).then_some((0, column_ix, rows - 1, column_ix));
    self.selected_column_band = Some((column_ix, column_ix));
    self.selected_row_band = None;
    let row = self.cursor.map(|(row, _)| row).unwrap_or(0);
    self.cursor = Some((row, column_ix));
    self.selection_anchor = Some((row, column_ix));
    cx.notify();
  }

  /// Header shift-click: select every cell across a span of columns.
  pub fn select_column_span(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
    let (c0, c1) = (from.min(to), from.max(to));
    let Some(sheet) = self.active_sheet_ref() else { return };
    let rows = sheet.rows.len();
    let set: std::collections::HashSet<CellId> = sheet
      .rows
      .iter()
      .flat_map(|row| (c0..=c1).filter_map(|c| row.cells.get(c).and_then(|slot| slot.as_ref()).map(|cell| cell.id)))
      .collect();
    self.selected_cells = set;
    self.selection_rect = (rows > 0).then_some((0, c0, rows - 1, c1));
    self.selected_column_band = Some((c0, c1));
    self.selected_row_band = None;
    cx.notify();
  }

  /// Header ctrl-click: add a whole column's cells to the current selection.
  pub fn add_column_to_selection(&mut self, column_ix: usize, cx: &mut Context<Self>) {
    let Some(sheet) = self.active_sheet_ref() else { return };
    let ids: Vec<CellId> = sheet
      .rows
      .iter()
      .filter_map(|row| row.cells.get(column_ix).and_then(|slot| slot.as_ref()).map(|cell| cell.id))
      .collect();
    self.selected_cells.extend(ids);
    self.clear_selection_shape();
    self.selection_anchor = Some((0, column_ix));
    cx.notify();
  }

  /// Gutter shift-click: select every cell across a span of rows.
  pub fn select_row_span(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
    let (r0, r1) = (from.min(to), from.max(to));
    let Some(sheet) = self.active_sheet_ref() else { return };
    let columns = sheet.columns.len();
    let set: std::collections::HashSet<CellId> = (r0..=r1)
      .filter_map(|r| sheet.rows.get(r))
      .flat_map(|row| row.cells.iter().filter_map(|slot| slot.as_ref().map(|cell| cell.id)))
      .collect();
    self.selected_cells = set;
    self.selection_rect = (columns > 0).then_some((r0, 0, r1, columns - 1));
    self.selected_row_band = Some((r0, r1));
    self.selected_column_band = None;
    self.cursor = Some((r1, 0));
    cx.notify();
  }

  /// Gutter ctrl-click: add a whole row's cells to the current selection.
  pub fn add_row_to_selection(&mut self, row_ix: usize, cx: &mut Context<Self>) {
    let Some(sheet) = self.active_sheet_ref() else { return };
    let ids: Vec<CellId> = sheet
      .rows
      .get(row_ix)
      .into_iter()
      .flat_map(|row| row.cells.iter().filter_map(|slot| slot.as_ref().map(|cell| cell.id)))
      .collect();
    self.selected_cells.extend(ids);
    self.clear_selection_shape();
    self.selection_anchor = Some((row_ix, 0));
    cx.notify();
  }

  /// Ctrl+A (and the top-left corner box): select every cell in the sheet.
  pub fn select_all(&mut self, cx: &mut Context<Self>) {
    let Some(sheet) = self.active_sheet_ref() else { return };
    let (rows, columns) = (sheet.rows.len(), sheet.columns.len());
    let set: std::collections::HashSet<CellId> = sheet
      .rows
      .iter()
      .flat_map(|row| row.cells.iter().filter_map(|slot| slot.as_ref().map(|cell| cell.id)))
      .collect();
    self.selected_cells = set;
    self.selection_rect = (rows > 0 && columns > 0).then_some((0, 0, rows - 1, columns - 1));
    self.selected_row_band = None;
    self.selected_column_band = None;
    cx.notify();
  }

  /// F2 / Enter-to-edit: drop keyboard focus into the cursor's occupied cell,
  /// caret at the end (the keyboard twin of a single click).
  pub fn edit_cursor_cell(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
    let Some((row_ix, column_ix)) = self.cursor else { return };
    match self.active_sheet_ref().and_then(|sheet| sheet.slot(row_ix, column_ix)).map(|cell| cell.id) {
      Some(cell_id) => {
        self.activate_cell(cell_id, cx);
        self.ensure_cell_editor(cell_id, cx);
        self.focus_active_cell(window, cx);
      },
      // Excel: F2 on a BLANK slot enters edit mode on a fresh empty cell —
      // never a silent no-op inconsistent with an occupied cell.
      None => {
        if let Some(cell_id) = self.add_cell_at_slot(row_ix, column_ix, CellSeed::Empty, cx) {
          self.activate_cell(cell_id, cx);
          self.focus_active_cell(window, cx);
        }
      },
    }
  }

  /// Type-to-overwrite (Excel): a printable key on an occupied cell REPLACES
  /// its content, seeded with that key; on an empty slot it creates (the
  /// existing `type_into_cursor` path). One undo group either way.
  pub fn overwrite_cursor(&mut self, text: &str, cx: &mut Context<Self>) -> Option<CellId> {
    let (row_ix, column_ix) = self.cursor?;
    let sheet_id = self.active_sheet?;
    let occupant = self.active_sheet_ref()?.slot(row_ix, column_ix).map(|cell| cell.id);
    match occupant {
      None => self.type_into_cursor(text, cx),
      Some(cell_id) => {
        let seed = CellSeed::Paragraphs(vec![flowstate_document::InputParagraph {
          style: flowstate_document::PARAGRAPH_TAG,
          runs: vec![flowstate_document::InputRun {
            text: text.to_string(),
            styles: flowstate_document::RunStyles::default(),
          }],
        }]);
        self.begin_bulk();
        let _ = self.apply_intent(&FlowIntent::DeleteCell { sheet_id, cell_id }, cx);
        let created = self.add_cell_at_slot(row_ix, column_ix, seed, cx);
        self.end_bulk("edit", cx);
        created
      },
    }
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
