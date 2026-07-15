//! Living Grid keyboard layer (flow spec §3.2): Ctrl/Cmd-Enter new family,
//! Ctrl-arrow spreadsheet navigation, Shift-Alt-arrow movement — plus the
//! refusal voice (§3.1 F3): every refused move states its reason instead of
//! silently doing nothing.

use flowstate_flow::{CellId, CellPlacement, FlowDropIntent, FlowIntent, Sheet};
use gpui::Context;

use super::FlowEditor;
use super::layout::sheet_cell_layout;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridDirection {
  Up,
  Down,
  Left,
  Right,
}

impl FlowEditor {
  /// The active sheet's projection, or `None` with no board.
  fn active_sheet_ref(&self) -> Option<&Sheet> {
    let sheet_id = self.active_sheet?;
    self.board.sheets.iter().find(|sheet| sheet.id == sheet_id)
  }

  /// Column index of a cell within its sheet's type definition.
  fn column_index_of(&self, sheet: &Sheet, cell_id: CellId) -> Option<usize> {
    let cell = sheet.cells.iter().find(|cell| cell.id == cell_id)?;
    self
      .board
      .format
      .sheet_type(sheet.sheet_type_id)?
      .columns
      .iter()
      .position(|column| column.id == cell.column_id)
  }

  /// This column's cells in VISUAL (layout-top) order — the order the user
  /// sees, which diverges from sheet order after moves.
  fn column_cells_by_top(&self, sheet: &Sheet, column_index: usize) -> Vec<(CellId, f32)> {
    let Some(column_id) = self
      .board
      .format
      .sheet_type(sheet.sheet_type_id)
      .and_then(|definition| definition.columns.get(column_index))
      .map(|column| column.id)
    else {
      return Vec::new();
    };
    let layout = sheet_cell_layout(sheet, &self.cell_measurements, self.board_zoom);
    let mut cells: Vec<(CellId, f32)> = sheet
      .cells
      .iter()
      .filter(|cell| cell.column_id == column_id)
      .map(|cell| (cell.id, layout.get(&cell.id).map_or(0.0, |layout| layout.top)))
      .collect();
    cells.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    cells
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

  /// W3 — Ctrl/Cmd-Enter: a NEW FAMILY in the active column (completes the
  /// Enter grammar: sibling / above / answer / family).
  pub fn add_new_family(&mut self, cx: &mut Context<Self>) {
    let Some(sheet) = self.active_sheet_ref() else {
      return;
    };
    let sheet_id = sheet.id;
    let column_index = self
      .active_cell
      .and_then(|cell| self.column_index_of(sheet, cell))
      .unwrap_or(0);
    self.add_cell(sheet_id, CellPlacement::SheetEnd { column_index }, cx);
  }

  /// R4 — Ctrl-arrows: ↑↓ walk the column in visual order; ←→ jump to the
  /// NEAREST card (by vertical position) in the adjacent column.
  pub fn navigate(&mut self, direction: GridDirection, cx: &mut Context<Self>) {
    let Some(sheet) = self.active_sheet_ref() else {
      return;
    };
    let target = match self.active_cell {
      Some(active) => self.navigation_target(sheet, active, direction),
      // No selection yet: land on the first card of the first column.
      None => self
        .column_cells_by_top(sheet, 0)
        .first()
        .map(|(id, _)| *id),
    };
    if let Some(target) = target {
      self.activate_cell(target, cx);
    }
  }

  fn navigation_target(&self, sheet: &Sheet, active: CellId, direction: GridDirection) -> Option<CellId> {
    let column_index = self.column_index_of(sheet, active)?;
    let column = self.column_cells_by_top(sheet, column_index);
    let position = column.iter().position(|(id, _)| *id == active)?;
    match direction {
      GridDirection::Up => position
        .checked_sub(1)
        .and_then(|previous| column.get(previous))
        .map(|(id, _)| *id),
      GridDirection::Down => column.get(position + 1).map(|(id, _)| *id),
      GridDirection::Left | GridDirection::Right => {
        let adjacent = if direction == GridDirection::Left {
          column_index.checked_sub(1)?
        } else {
          column_index + 1
        };
        let top = column[position].1;
        self
          .column_cells_by_top(sheet, adjacent)
          .into_iter()
          .min_by(|a, b| {
            (a.1 - top)
              .abs()
              .partial_cmp(&(b.1 - top).abs())
              .unwrap_or(std::cmp::Ordering::Equal)
          })
          .map(|(id, _)| id)
      },
    }
  }

  /// R9 — Shift-Alt-arrows: ↑↓ nudge within the sibling run; ←→ shift the
  /// card a generation (wire re-plug to the nearest valid parent), refusing
  /// WITH WORDS when no valid landing exists.
  pub fn move_active_cell(&mut self, direction: GridDirection, cx: &mut Context<Self>) {
    let Some((sheet_id, active)) = self.active_sheet.zip(self.active_cell) else {
      return;
    };
    let drop_result = {
      let Some(sheet) = self.active_sheet_ref() else {
        return;
      };
      self.movement_drop(sheet, active, direction)
    };
    let drop = match drop_result {
      Ok(drop) => drop,
      Err(reason) => {
        self.refuse(reason, Some(active), cx);
        return;
      },
    };
    if self
      .apply_intent(
        &FlowIntent::MoveCellSubtree {
          sheet_id,
          cell_id: active,
          drop,
        },
        cx,
      )
      .is_ok()
    {
      self.changed(Some(active), cx);
      self.scroll_cell_into_view(active);
    }
  }

  fn movement_drop(&self, sheet: &Sheet, active: CellId, direction: GridDirection) -> Result<FlowDropIntent, String> {
    let cell = sheet
      .cells
      .iter()
      .find(|cell| cell.id == active)
      .ok_or_else(|| "the selected card no longer exists".to_string())?;
    match direction {
      GridDirection::Up | GridDirection::Down => {
        // Siblings = same parent AND same column, in sheet order (the run).
        let run: Vec<CellId> = sheet
          .cells
          .iter()
          .filter(|candidate| candidate.parent_id == cell.parent_id && candidate.column_id == cell.column_id)
          .map(|candidate| candidate.id)
          .collect();
        let position = run
          .iter()
          .position(|id| *id == active)
          .ok_or_else(|| "the selected card no longer exists".to_string())?;
        if direction == GridDirection::Up {
          position
            .checked_sub(1)
            .and_then(|previous| run.get(previous))
            .map(|previous| FlowDropIntent::BeforeSibling(*previous))
            .ok_or_else(|| "already first in its run".to_string())
        } else {
          run
            .get(position + 1)
            .map(|next| FlowDropIntent::AfterSibling(*next))
            .ok_or_else(|| "already last in its run".to_string())
        }
      },
      GridDirection::Left => {
        // Promote: become the answer's sibling — land right after the parent.
        cell
          .parent_id
          .map(FlowDropIntent::AfterSibling)
          .ok_or_else(|| "already a root card — no earlier column to join".to_string())
      },
      GridDirection::Right => {
        // Demote: re-plug to the nearest valid parent in this column (a card
        // above it that is not inside its own thread).
        let column_index = self
          .column_index_of(sheet, active)
          .ok_or_else(|| "the selected card no longer exists".to_string())?;
        let column_ids: Vec<_> = self
          .board
          .format
          .sheet_type(sheet.sheet_type_id)
          .map(|definition| definition.columns.iter().map(|column| column.id).collect())
          .unwrap_or_default();
        if column_index + 1 >= column_ids.len() {
          return Err("already in the last column — no deeper generation exists".to_string());
        }
        let column = self.column_cells_by_top(sheet, column_index);
        let position = column
          .iter()
          .position(|(id, _)| *id == active)
          .ok_or_else(|| "the selected card no longer exists".to_string())?;
        let top = column[position].1;
        column
          .iter()
          .filter(|(id, _)| *id != active && !flowstate_flow::board_ops::is_descendant_of(sheet, *id, active))
          .min_by(|a, b| {
            (a.1 - top)
              .abs()
              .partial_cmp(&(b.1 - top).abs())
              .unwrap_or(std::cmp::Ordering::Equal)
          })
          .map(|(parent, _)| FlowDropIntent::LastChildOf(*parent))
          .ok_or_else(|| "no card in this column can take the answer".to_string())
      },
    }
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
}
