//! The Living Grid drag layer (flow spec §3.1): drag = POSITION. Slot capture
//! is column-full-height against a layout snapshot FROZEN at drag start (so
//! drop targets never move under the pointer), the live make-room reflow is
//! the preview (render paints the previewed board), ambiguity unfolds landing
//! pads at the gap (the default pad renders pre-selected; cursor-on-pad
//! re-selects; release commits), a meaning-chip rides the ghost only at
//! unambiguous spots (while pads are up they carry the meaning — a chip there
//! would cover the very options it names), and a fast release FLINGS the card
//! to the strongest-magnet valid slot along its trajectory.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;

use flowstate_flow::{CellId, FlowDropIntent, FlowIntent};
use gpui::{Bounds, Context, IntoElement, Pixels, Point, Render, SharedString, WeakEntity, Window, div, point, prelude::*, px};
use gpui_component::ActiveTheme as _;
use gpui_component::PixelsExt as _;

use super::FlowEditor;

/// R8: release faster than this projects the trajectory instead of dropping
/// in place.
const FLING_SPEED: f32 = 850.0;
/// How far ahead (seconds) the fling projects the pointer.
const FLING_PROJECTION: f32 = 0.15;

#[derive(Clone)]
pub(super) struct FlowCellDrag {
  pub(super) cell_id: CellId,
}

/// G5/W1: dragging a wire's fat plug to a new parent. The payload names the
/// CHILD whose incoming wire is being re-plugged.
#[derive(Clone)]
pub(super) struct WirePlugDrag {
  pub(super) cell_id: CellId,
}

/// The plug ghost riding the cursor during a re-plug.
pub(super) struct WirePlugPreview;

impl Render for WirePlugPreview {
  fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    div()
      .size(px(14.0))
      .rounded_full()
      .bg(cx.theme().primary)
      .border_2()
      .border_color(cx.theme().primary_foreground.opacity(0.7))
  }
}

/// One landing-pad option at an ambiguous gap (R1/R5).
#[derive(Clone)]
pub(super) struct PadOption {
  pub(super) drop: FlowDropIntent,
  pub(super) label: SharedString,
}

/// Live drag bookkeeping, snapshotted at drag start. Slot capture runs
/// against `baseline` — the pre-drag geometry — NEVER the reflowing preview,
/// so capture zones are stable by construction.
pub(super) struct DragSession {
  pub(super) baseline: HashMap<CellId, Bounds<Pixels>>,
  pub(super) subtree: HashSet<CellId>,
  /// Landing pads for the current ambiguous slot (empty = unambiguous).
  pub(super) pads: Vec<PadOption>,
  /// Window-space anchor of the pad row + its captured hover region.
  pub(super) pad_origin: Option<Point<Pixels>>,
  pub(super) pad_rect: Option<Bounds<Pixels>>,
  /// The pad release would commit: the slot's default until the pointer
  /// claims another pad. Always Some while pads are up.
  pub(super) selected_pad: Option<usize>,
  /// Pointer samples for fling velocity (position, instant).
  pub(super) samples: VecDeque<(Point<Pixels>, Instant)>,
  /// The chip riding the ghost, updated as the meaning changes.
  pub(super) preview: Option<WeakEntity<FlowCellDragPreview>>,
}

/// The cursor ghost: the dragged card (stacked when a thread rides along,
/// with a "+N" census badge — F1) plus the meaning-chip stating exactly what
/// release will do. The chip only shows at unambiguous spots (empty meaning =
/// hidden); when landing pads are up, the selected pad states the meaning.
pub(super) struct FlowCellDragPreview {
  pub(super) label: SharedString,
  pub(super) census: usize,
  pub(super) meaning: SharedString,
}

impl Render for FlowCellDragPreview {
  fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    let stacked = self.census > 0;
    div()
      .flex()
      .flex_col()
      .gap_1()
      // The held card must never fully obscure what it hovers (§3.1 render-
      // order law; gpui paints the drag ghost last, so translucency stands in
      // for true z-ordering).
      .opacity(0.88)
      .child(
        div()
          .relative()
          .max_w(px(280.0))
          // F1: the subtree ghost reads as a STACK — offset echo cards behind
          // the lead card.
          .when(stacked, |this| {
            this
              .child(
                div()
                  .absolute()
                  .top(px(6.0))
                  .left(px(6.0))
                  .right(px(-6.0))
                  .bottom(px(-6.0))
                  .rounded(cx.theme().radius)
                  .bg(cx.theme().popover.opacity(0.55))
                  .border_1()
                  .border_color(cx.theme().border.opacity(0.55)),
              )
              .child(
                div()
                  .absolute()
                  .top(px(3.0))
                  .left(px(3.0))
                  .right(px(-3.0))
                  .bottom(px(-3.0))
                  .rounded(cx.theme().radius)
                  .bg(cx.theme().popover.opacity(0.75))
                  .border_1()
                  .border_color(cx.theme().border.opacity(0.75)),
              )
          })
          .child(
            div()
              .relative()
              .px_2()
              .py_1()
              .rounded(cx.theme().radius)
              .bg(cx.theme().popover)
              .border_1()
              .border_color(cx.theme().border)
              .text_color(cx.theme().popover_foreground)
              .overflow_hidden()
              .child(if self.label.is_empty() {
                SharedString::from("Argument")
              } else {
                self.label.clone()
              })
              .when(stacked, |this| {
                this.child(
                  div()
                    .absolute()
                    .top(px(-6.0))
                    .right(px(-6.0))
                    .px(px(6.0))
                    .rounded_full()
                    .bg(cx.theme().primary)
                    .text_color(cx.theme().primary_foreground)
                    .text_size(px(11.0))
                    .child(format!("+{}", self.census)),
                )
              }),
          ),
      )
      // The meaning-chip: exactly what release will do (§3.1).
      .when(!self.meaning.is_empty(), |this| {
        this.child(
          div()
            .px_2()
            .py_0p5()
            .rounded_full()
            .bg(cx.theme().primary.opacity(0.9))
            .text_color(cx.theme().primary_foreground)
            .text_size(px(11.0))
            .child(self.meaning.clone()),
        )
      })
  }
}

impl FlowEditor {
  /// Drag start (past the movement threshold): freeze the slot-capture
  /// baseline and record the riding thread.
  pub(super) fn begin_cell_drag(&mut self, cell_id: CellId, cx: &mut Context<Self>) {
    if self.dragging_cell == Some(cell_id) {
      return;
    }
    self.dragging_cell = Some(cell_id);
    self.pending_cell_drop = None;
    let subtree: HashSet<CellId> = self
      .active_sheet
      .and_then(|sheet_id| self.board.sheets.iter().find(|sheet| sheet.id == sheet_id))
      .map(|sheet| {
        // W2: dragging a selected card carries the WHOLE selection.
        if self.selected_cells.contains(&cell_id) && self.selected_cells.len() > 1 {
          self
            .selected_cells
            .iter()
            .flat_map(|selected| flowstate_flow::board_ops::subtree_cell_ids(sheet, *selected))
            .collect()
        } else {
          flowstate_flow::board_ops::subtree_cell_ids(sheet, cell_id)
            .into_iter()
            .collect()
        }
      })
      .unwrap_or_default();
    self.drag = Some(DragSession {
      baseline: self.cell_bounds.clone(),
      subtree,
      pads: Vec::new(),
      pad_origin: None,
      pad_rect: None,
      selected_pad: None,
      samples: VecDeque::with_capacity(16),
      preview: None,
    });
    self.start_drag_log(cell_id);
    cx.notify();
  }

  pub(super) fn drag_subtree_contains(&self, cell_id: CellId) -> bool {
    self
      .drag
      .as_ref()
      .is_some_and(|drag| drag.subtree.contains(&cell_id))
  }

  /// The census badge count for the ghost (thread size beyond the lead card).
  pub(super) fn drag_census(&self, cell_id: CellId) -> usize {
    self
      .active_sheet
      .and_then(|sheet_id| self.board.sheets.iter().find(|sheet| sheet.id == sheet_id))
      .map(|sheet| {
        if self.selected_cells.contains(&cell_id) && self.selected_cells.len() > 1 {
          self
            .selected_cells
            .iter()
            .flat_map(|selected| flowstate_flow::board_ops::subtree_cell_ids(sheet, *selected))
            .collect::<HashSet<_>>()
            .len()
            .saturating_sub(1)
        } else {
          flowstate_flow::board_ops::subtree_cell_ids(sheet, cell_id)
            .len()
            .saturating_sub(1)
        }
      })
      .unwrap_or(0)
  }

  /// THE slot-capture handler (board-level `on_drag_move`). Resolves pointer
  /// → column (full-height x capture; only true inter-column space is dead)
  /// → slot in the FROZEN baseline → meaning set; pads render when the slot
  /// plausibly means more than one parentage.
  pub(super) fn update_drag_slot(&mut self, board_bounds: Bounds<Pixels>, position: Point<Pixels>, cx: &mut Context<Self>) {
    let Some(dragged) = self.dragging_cell else {
      return;
    };
    // Velocity sample first — fling needs motion history even over dead
    // space and pads.
    if let Some(drag) = self.drag.as_mut() {
      drag.samples.push_back((position, Instant::now()));
      while drag.samples.len() > 12 {
        drag.samples.pop_front();
      }
      // Pointer inside the unfolded pad row: FREEZE slot recomputation — the
      // pads own the meaning until the pointer leaves their region.
      if drag.pad_rect.is_some_and(|rect| rect.contains(&position)) {
        return;
      }
      drag.selected_pad = None;
    }
    let zoom = self.board_zoom;
    let column_width = super::layout::COLUMN_WIDTH * zoom;
    let stride = (super::layout::COLUMN_WIDTH + super::layout::COLUMN_GAP) * zoom;
    let relative = (position.x - board_bounds.left()).as_f32() - super::layout::BOARD_PADDING * zoom;
    let column_count = self.active_column_count();
    let raw_index = (relative / stride).floor().max(0.0);
    let within = relative - raw_index * stride;
    let index = raw_index as usize;
    if relative >= 0.0 && index < column_count && within > column_width {
      // True inter-column space: dead (spec §3.1) — hold the last capture.
      return;
    }
    let column_index = index.min(column_count.saturating_sub(1));
    let Some((options, default_option, pad_anchor)) = self.slot_meanings(dragged, column_index, position) else {
      return;
    };
    let ambiguous = options.len() > 1;
    let default_index = options.iter().position(|option| option.drop == default_option.drop);
    if let Some(drag) = self.drag.as_mut() {
      if ambiguous {
        drag.pads = options.clone();
        drag.pad_origin = Some(pad_anchor);
        // The slot's default meaning reads as the pre-selected pad — the pads
        // carry the meaning while they're up, so the chip stays hidden (it
        // would pop up exactly where the row unfolds and cover the options).
        drag.selected_pad = default_index;
        // The hover region is captured generously; the exact rect is refined
        // by the pad row's own painted bounds on hover.
        if drag.pad_rect.is_none() {
          drag.pad_rect = Some(Bounds::new(
            point(pad_anchor.x - px(16.0), pad_anchor.y - px(14.0)),
            gpui::size(px(560.0), px(56.0)),
          ));
        }
      } else {
        drag.pads = Vec::new();
        drag.pad_origin = None;
        drag.pad_rect = None;
      }
    }
    let default_drop = default_option.drop;
    self.set_drag_meaning(
      if ambiguous {
        SharedString::default()
      } else {
        default_option.label.clone()
      },
      cx,
    );
    if self.pending_cell_drop != Some(default_drop) {
      self.pending_cell_drop = Some(default_drop);
      self.log_drag_over_column(column_index, position, default_drop);
      cx.notify();
    }
  }

  /// A pad claimed the meaning (cursor-on-pad selects — R1/R5).
  pub(super) fn hover_pad(&mut self, pad_index: usize, pad_row_bounds: Bounds<Pixels>, cx: &mut Context<Self>) {
    let Some(drag) = self.drag.as_mut() else {
      return;
    };
    let Some(option) = drag.pads.get(pad_index).cloned() else {
      return;
    };
    drag.selected_pad = Some(pad_index);
    // Refine the freeze region to the actual painted row (+ margin).
    drag.pad_rect = Some(Bounds::new(
      point(pad_row_bounds.left() - px(12.0), pad_row_bounds.top() - px(12.0)),
      gpui::size(pad_row_bounds.size.width + px(24.0), pad_row_bounds.size.height + px(24.0)),
    ));
    if self.pending_cell_drop != Some(option.drop) {
      self.pending_cell_drop = Some(option.drop);
      cx.notify();
    }
  }

  fn set_drag_meaning(&mut self, meaning: SharedString, cx: &mut Context<Self>) {
    if let Some(preview) = self.drag.as_ref().and_then(|drag| drag.preview.clone()) {
      let _ = preview.update(cx, |preview, cx| {
        if preview.meaning != meaning {
          preview.meaning = meaning;
          cx.notify();
        }
      });
    }
  }

  pub(super) fn set_drag_preview_entity(&mut self, entity: WeakEntity<FlowCellDragPreview>) {
    if let Some(drag) = self.drag.as_mut() {
      drag.preview = Some(entity);
    }
  }

  fn active_column_count(&self) -> usize {
    self
      .active_sheet
      .and_then(|sheet_id| self.board.sheets.iter().find(|sheet| sheet.id == sheet_id))
      .and_then(|sheet| self.board.format.sheet_type(sheet.sheet_type_id))
      .map_or(0, |definition| definition.columns.len())
  }

  /// Enumerate the meanings a slot plausibly carries (the parentCandidates
  /// law): join the anchor's run / root here / answer the previous-column
  /// card at this height / answer the anchor itself. Calibration #6: if a
  /// location plausibly means X, X's pad must be present. Returns
  /// (all options, the default option, pad-row anchor in window space).
  fn slot_meanings(&self, dragged: CellId, column_index: usize, position: Point<Pixels>) -> Option<(Vec<PadOption>, PadOption, Point<Pixels>)> {
    let sheet = self
      .active_sheet
      .and_then(|sheet_id| self.board.sheets.iter().find(|sheet| sheet.id == sheet_id))?;
    let definition = self.board.format.sheet_type(sheet.sheet_type_id)?;
    let column_id = definition.columns.get(column_index)?.id;
    let column_ids: Vec<_> = definition.columns.iter().map(|column| column.id).collect();
    let drag = self.drag.as_ref()?;

    // The frozen baseline for this column, in visual order, minus the thread
    // riding the cursor.
    let mut column_cells: Vec<(CellId, Bounds<Pixels>)> = sheet
      .cells
      .iter()
      .filter(|cell| cell.column_id == column_id && !drag.subtree.contains(&cell.id))
      .filter_map(|cell| drag.baseline.get(&cell.id).map(|bounds| (cell.id, *bounds)))
      .collect();
    column_cells.sort_by(|a, b| {
      a.1
        .top()
        .partial_cmp(&b.1.top())
        .unwrap_or(std::cmp::Ordering::Equal)
    });
    let above = column_cells
      .iter()
      .rfind(|(_, bounds)| bounds.center().y <= position.y)
      .map(|(id, bounds)| (*id, *bounds));
    let below = column_cells
      .iter()
      .find(|(_, bounds)| bounds.center().y > position.y)
      .map(|(id, bounds)| (*id, *bounds));

    let valid = |drop: FlowDropIntent| flowstate_flow::board_ops::preview_move_cell_subtree(sheet, &column_ids, dragged, drop).is_some();
    let cell_of = |id: CellId| sheet.cells.iter().find(|cell| cell.id == id);
    let label_of = |id: CellId| self.cell_label(id);

    // The pad row unfolds AT the gap.
    let pad_anchor = match (&above, &below) {
      (Some((_, above_bounds)), Some((_, below_bounds))) => point(above_bounds.left(), (above_bounds.bottom() + below_bounds.top()) / 2.0),
      (Some((_, above_bounds)), None) => point(above_bounds.left(), above_bounds.bottom() + px(8.0)),
      (None, Some((_, below_bounds))) => point(below_bounds.left(), below_bounds.top() - px(8.0)),
      (None, None) => position,
    };

    let mut options: Vec<PadOption> = Vec::new();
    fn push_option(options: &mut Vec<PadOption>, valid: &impl Fn(FlowDropIntent) -> bool, drop: FlowDropIntent, label: String) {
      if valid(drop) && !options.iter().any(|option| option.drop == drop) {
        options.push(PadOption { drop, label: label.into() });
      }
    }
    macro_rules! push {
      ($drop:expr, $label:expr) => {
        push_option(&mut options, &valid, $drop, $label)
      };
    }

    // Unambiguous middle-of-a-run: both neighbors share a parent — the slot
    // means exactly one thing (join that run). No pads (spec: unambiguous
    // spots never show pads).
    if let (Some((above_id, _)), Some((below_id, _))) = (&above, &below)
      && let (Some(above_cell), Some(below_cell)) = (cell_of(*above_id), cell_of(*below_id))
      && above_cell.parent_id == below_cell.parent_id
    {
      push!(
        FlowDropIntent::BeforeSibling(*below_id),
        format!("Into the run — before “{}”", label_of(*below_id))
      );
      if options.len() == 1 {
        let only = options[0].clone();
        return Some((options, only, pad_anchor));
      }
    }

    // Join the anchor's run (DEFAULT — run semantics).
    if let Some((above_id, _)) = above {
      push!(FlowDropIntent::AfterSibling(above_id), format!("Join “{}”'s run", label_of(above_id)));
    }
    if let Some((below_id, _)) = below {
      push!(
        FlowDropIntent::BeforeSibling(below_id),
        format!("Join “{}”'s run (above it)", label_of(below_id))
      );
    }
    // Root here (new family at this position).
    let insertion_index = below
      .and_then(|(below_id, _)| sheet.cells.iter().position(|cell| cell.id == below_id))
      .unwrap_or(sheet.cells.len());
    push!(
      FlowDropIntent::RootInColumn {
        column_index,
        insertion_index,
      },
      "New family here".to_string()
    );
    // Answer the previous-column card at this height.
    if let Some(previous_column_id) = column_index
      .checked_sub(1)
      .and_then(|previous| definition.columns.get(previous))
      .map(|column| column.id)
    {
      let previous_at_height = sheet
        .cells
        .iter()
        .filter(|cell| cell.column_id == previous_column_id && !drag.subtree.contains(&cell.id))
        .filter_map(|cell| drag.baseline.get(&cell.id).map(|bounds| (cell.id, *bounds)))
        .find(|(_, bounds)| position.y >= bounds.top() && position.y <= bounds.bottom())
        .map(|(id, _)| id);
      if let Some(parent) = previous_at_height {
        push!(FlowDropIntent::LastChildOf(parent), format!("Answer “{}”", label_of(parent)));
      }
    }
    // Answer the anchor itself (the card just above the gap).
    if let Some((above_id, _)) = above
      && column_index + 1 < definition.columns.len()
    {
      push!(FlowDropIntent::FirstChildOf(above_id), format!("Answer “{}” itself", label_of(above_id)));
    }

    let default_option = options.first().cloned()?;
    Some((options, default_option, pad_anchor))
  }

  /// While dragging near a viewport edge, scroll the board toward it. Driven by a self-rescheduling
  /// frame loop so scrolling continues even when the pointer holds still at the edge.
  pub(super) fn update_drag_autoscroll(&mut self, pointer: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
    let bounds = self.board_scroll.bounds();
    if bounds.size.width <= px(1.0) || bounds.size.height <= px(1.0) {
      return;
    }
    const MARGIN: f32 = 56.0;
    const MAX_SPEED: f32 = 24.0;
    let ramp = |distance: f32| -> f32 {
      if distance <= 0.0 {
        MAX_SPEED
      } else if distance < MARGIN {
        MAX_SPEED * (1.0 - distance / MARGIN)
      } else {
        0.0
      }
    };
    let vx = ramp((pointer.x - bounds.left()).as_f32()) - ramp((bounds.right() - pointer.x).as_f32());
    let vy = ramp((pointer.y - bounds.top()).as_f32()) - ramp((bounds.bottom() - pointer.y).as_f32());
    if vx == 0.0 && vy == 0.0 {
      self.drag_autoscroll = None;
      return;
    }
    self.drag_autoscroll = Some(point(px(vx), px(vy)));
    self.schedule_drag_autoscroll(window, cx);
  }

  fn schedule_drag_autoscroll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    if self.drag_autoscroll_scheduled {
      return;
    }
    self.drag_autoscroll_scheduled = true;
    cx.on_next_frame(window, |editor, window, cx| {
      editor.drag_autoscroll_scheduled = false;
      let Some(velocity) = editor.drag_autoscroll else {
        return;
      };
      if !cx.has_active_drag() || editor.dragging_cell.is_none() {
        editor.drag_autoscroll = None;
        return;
      }
      editor.set_user_scroll_offset(editor.board_scroll.offset() + velocity);
      cx.notify();
      editor.schedule_drag_autoscroll(window, cx);
    });
  }

  /// R8: pointer velocity from the sample tail (px/s), if the release was a
  /// genuine motion (not a rest).
  fn release_velocity(&self) -> Option<Point<Pixels>> {
    let drag = self.drag.as_ref()?;
    let (last_position, last_at) = *drag.samples.back()?;
    // Use the sample ~50ms+ back for a stable estimate.
    let (base_position, base_at) = drag
      .samples
      .iter()
      .rev()
      .find(|(_, at)| last_at.duration_since(*at).as_secs_f32() >= 0.05)
      .copied()?;
    let dt = last_at.duration_since(base_at).as_secs_f32();
    if dt <= f32::EPSILON {
      return None;
    }
    Some(point(
      px((last_position.x - base_position.x).as_f32() / dt),
      px((last_position.y - base_position.y).as_f32() / dt),
    ))
  }

  /// R8: on a fast release, project the trajectory and let the strongest
  /// magnet — the nearest VALID slot to the projected point, anywhere — take
  /// the card. Flings never summon pads (run semantics).
  fn fling_target(&self, dragged: CellId) -> Option<FlowDropIntent> {
    let velocity = self.release_velocity()?;
    let speed = velocity.x.as_f32().hypot(velocity.y.as_f32());
    if speed < FLING_SPEED {
      return None;
    }
    let drag = self.drag.as_ref()?;
    let (last_position, _) = *drag.samples.back()?;
    let projected = point(
      last_position.x + px(velocity.x.as_f32() * FLING_PROJECTION),
      last_position.y + px(velocity.y.as_f32() * FLING_PROJECTION),
    );
    let sheet = self
      .active_sheet
      .and_then(|sheet_id| self.board.sheets.iter().find(|sheet| sheet.id == sheet_id))?;
    let definition = self.board.format.sheet_type(sheet.sheet_type_id)?;
    let column_ids: Vec<_> = definition.columns.iter().map(|column| column.id).collect();
    // Candidate magnets: every run slot around every baseline card.
    let mut best: Option<(f32, FlowDropIntent)> = None;
    for cell in &sheet.cells {
      if drag.subtree.contains(&cell.id) {
        continue;
      }
      let Some(bounds) = drag.baseline.get(&cell.id) else {
        continue;
      };
      for (drop, anchor_y) in [
        (FlowDropIntent::BeforeSibling(cell.id), bounds.top()),
        (FlowDropIntent::AfterSibling(cell.id), bounds.bottom()),
      ] {
        let distance = (projected.x - bounds.center().x)
          .as_f32()
          .hypot((projected.y - anchor_y).as_f32());
        if best
          .as_ref()
          .is_none_or(|(best_distance, _)| distance < *best_distance)
          && flowstate_flow::board_ops::preview_move_cell_subtree(sheet, &column_ids, dragged, drop).is_some()
        {
          best = Some((distance, drop));
        }
      }
    }
    best.map(|(_, drop)| drop)
  }

  pub(super) fn finish_cell_drop(&mut self, dragged: CellId, cx: &mut Context<Self>) {
    // A fling overrides the hover slot (R8).
    let destination = self
      .fling_target(dragged)
      .or_else(|| self.pending_cell_drop.take());
    self.pending_cell_drop = None;
    self.dragging_cell = None;
    self.drag_autoscroll = None;
    // W2 group drag: the selected roots move as one set-op (cells whose
    // ancestor is also selected ride their subtree automatically).
    let group: Vec<CellId> = if self.drag.is_some() && self.selected_cells.contains(&dragged) && self.selected_cells.len() > 1 {
      self
        .active_sheet
        .and_then(|sheet_id| self.board.sheets.iter().find(|sheet| sheet.id == sheet_id))
        .map(|sheet| {
          sheet
            .cells
            .iter()
            .map(|cell| cell.id)
            .filter(|id| self.selected_cells.contains(id))
            .filter(|id| {
              !self
                .selected_cells
                .iter()
                .any(|other| other != id && flowstate_flow::board_ops::is_descendant_of(sheet, *id, *other))
            })
            .collect()
        })
        .unwrap_or_else(|| vec![dragged])
    } else {
      vec![dragged]
    };
    self.drag = None;
    let committed = match (destination, self.active_sheet) {
      (Some(destination), Some(sheet_id)) => {
        let lead_first: Vec<CellId> = std::iter::once(dragged)
          .chain(group.into_iter().filter(|id| *id != dragged))
          .collect();
        let count = lead_first.len();
        self.grouped(count, |editor| {
          let mut anchor = None;
          let mut any = false;
          for cell_id in lead_first {
            let drop = match anchor {
              None => destination,
              // Followers land as the lead's run, in order.
              Some(previous) => FlowDropIntent::AfterSibling(previous),
            };
            if editor
              .apply_intent(&FlowIntent::MoveCellSubtree { sheet_id, cell_id, drop }, cx)
              .is_ok()
            {
              anchor = Some(cell_id);
              any = true;
            }
          }
          any
        })
      },
      _ => false,
    };
    self.finish_drag_log(destination, committed);
    if committed {
      self.changed(Some(dragged), cx);
    } else {
      if destination.is_some() {
        // F3: a refused drop SPEAKS.
        self.refuse("that landing would break the thread — the move was refused", Some(dragged), cx);
      }
      cx.notify();
    }
  }
}

impl FlowEditor {
  /// Whether `target` can adopt the re-plugged cell: one column to the LEFT
  /// of the child (wires only span adjacent columns) and not inside the
  /// child's own thread.
  pub(super) fn replug_candidate(&self, target: CellId) -> bool {
    let Some(replug) = self.replug_cell else {
      return false;
    };
    let Some(sheet) = self
      .active_sheet
      .and_then(|sheet_id| self.board.sheets.iter().find(|sheet| sheet.id == sheet_id))
    else {
      return false;
    };
    let Some(definition) = self.board.format.sheet_type(sheet.sheet_type_id) else {
      return false;
    };
    let column_of = |id: CellId| {
      sheet
        .cells
        .iter()
        .find(|cell| cell.id == id)
        .and_then(|cell| {
          definition
            .columns
            .iter()
            .position(|column| column.id == cell.column_id)
        })
    };
    let (Some(child_column), Some(target_column)) = (column_of(replug), column_of(target)) else {
      return false;
    };
    target != replug && target_column + 1 == child_column && !flowstate_flow::board_ops::is_descendant_of(sheet, target, replug)
  }

  pub(super) fn begin_wire_replug(&mut self, cell_id: CellId, cx: &mut Context<Self>) {
    self.replug_cell = Some(cell_id);
    cx.notify();
  }

  /// Drop the plug on a candidate: the child (and thread) re-plugs as the
  /// target's answer. Invalid targets refuse WITH WORDS (F3).
  pub(super) fn finish_wire_replug(&mut self, child: CellId, target: CellId, cx: &mut Context<Self>) {
    self.replug_cell = None;
    let Some(sheet_id) = self.active_sheet else {
      return;
    };
    if !{
      self.replug_cell = Some(child); // re-arm for the validity check
      let valid = self.replug_candidate(target);
      self.replug_cell = None;
      valid
    } {
      self.refuse("a wire can only re-plug to the previous column, outside its own thread", Some(child), cx);
      return;
    }
    if self
      .apply_intent(
        &FlowIntent::MoveCellSubtree {
          sheet_id,
          cell_id: child,
          drop: FlowDropIntent::LastChildOf(target),
        },
        cx,
      )
      .is_ok()
    {
      self.changed(Some(child), cx);
    }
  }
}
