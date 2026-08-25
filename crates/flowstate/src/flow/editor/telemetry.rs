//! Env-gated drag-and-drop ergonomics telemetry.
//!
//! When `FLOWSTATE_DRAG_LOG` is set (to a path, or `1`/`true` for a default file), every cell drag is
//! recorded as one JSON object per line: the dragged cell, every zone/intent transition the pointer
//! passed through (with precise per-cell offsets), and the final committed result. Unset = zero cost.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Instant;

use flowstate_flow::{CellId, FlowDropIntent};
use gpui::{Bounds, Pixels, Point};
use gpui_component::PixelsExt as _;
use serde_json::{Value, json};

use super::FlowEditor;

/// In-flight recording for a single drag, flushed as one JSON line when the drag ends.
pub(super) struct DragLogSession {
  drag_id: u64,
  dragged: String,
  sheet: String,
  zoom: f32,
  started: Instant,
  last_key: Option<String>,
  samples: Vec<Value>,
}

fn log_path() -> Option<&'static PathBuf> {
  static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
  PATH
    .get_or_init(|| {
      std::env::var_os("FLOWSTATE_DRAG_LOG").and_then(|value| {
        let value = value.to_string_lossy();
        match value.trim() {
          "" | "0" | "false" => None,
          "1" | "true" => Some(PathBuf::from("flow-drag-telemetry.jsonl")),
          path => Some(PathBuf::from(path)),
        }
      })
    })
    .as_ref()
}

fn enabled() -> bool {
  log_path().is_some()
}

fn append_record(record: &Value) {
  let Some(path) = log_path() else {
    return;
  };
  match serde_json::to_string(record) {
    Ok(line) => {
      if let Err(error) = append_line(path, &line) {
        eprintln!("flow drag telemetry write failed: {error}");
      }
    },
    Err(error) => eprintln!("flow drag telemetry serialize failed: {error}"),
  }
}

fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
  use std::io::Write as _;
  let mut file = std::fs::OpenOptions::new()
    .create(true)
    .append(true)
    .open(path)?;
  writeln!(file, "{line}")
}

fn round(value: f32, places: u32) -> f32 {
  let scale = 10f32.powi(places as i32);
  (value * scale).round() / scale
}

impl FlowEditor {
  pub(super) fn start_drag_log(&mut self, dragged: CellId) {
    if !enabled() {
      self.drag_log = None;
      return;
    }
    let dragged = self.cell_label(dragged);
    let sheet = self
      .active_sheet
      .and_then(|id| self.board().sheet(id).map(|sheet| sheet.name.clone()))
      .unwrap_or_default();
    self.drag_log_counter += 1;
    self.drag_log = Some(DragLogSession {
      drag_id: self.drag_log_counter,
      dragged,
      sheet,
      zoom: self.board_zoom(),
      started: Instant::now(),
      last_key: None,
      samples: Vec::new(),
    });
  }

  pub(super) fn log_drag_over_cell(&mut self, over: CellId, cursor: Point<Pixels>, bounds: Bounds<Pixels>, intent: FlowDropIntent) {
    if self.drag_log.is_none() {
      return;
    }
    let over_label = self.cell_label(over);
    let intent_str = self.describe_intent(intent);
    let valid = self.landing_is_valid(intent);
    let width = bounds.size.width.as_f32().max(1.0);
    let height = bounds.size.height.as_f32().max(1.0);
    let offset_frac = [
      round((cursor.x.as_f32() - bounds.left().as_f32()) / width, 3),
      round((cursor.y.as_f32() - bounds.top().as_f32()) / height, 3),
    ];
    let key = format!("cell:{over_label}:{intent_str}");
    let over = json!({ "kind": "cell", "label": over_label, "offset_frac": offset_frac });
    self.push_sample(key, over, cursor, intent_str, valid);
  }

  pub(super) fn log_drag_over_column(&mut self, column_index: usize, cursor: Point<Pixels>, intent: FlowDropIntent) {
    if self.drag_log.is_none() {
      return;
    }
    let intent_str = self.describe_intent(intent);
    let valid = self.landing_is_valid(intent);
    let key = format!("column:{column_index}:{intent_str}");
    let over = json!({ "kind": "column", "index": column_index });
    self.push_sample(key, over, cursor, intent_str, valid);
  }

  pub(super) fn finish_drag_log(&mut self, intent: Option<FlowDropIntent>, committed: bool) {
    if self.drag_log.is_none() {
      return;
    }
    let intent = intent.map(|intent| self.describe_intent(intent));
    let result = self.sheet_topology_snapshot();
    let Some(session) = self.drag_log.take() else {
      return;
    };
    let record = json!({
      "drag_id": session.drag_id,
      "dragged": session.dragged,
      "sheet": session.sheet,
      "zoom": session.zoom,
      "duration_ms": session.started.elapsed().as_millis() as u64,
      "samples": session.samples,
      "drop": { "intent": intent, "committed": committed, "result": result },
    });
    append_record(&record);
  }

  fn push_sample(&mut self, key: String, over: Value, cursor: Point<Pixels>, intent: String, valid: bool) {
    let board_pt = [
      round((cursor.x.as_f32() - self.viewport_origin.x) / self.board_zoom(), 2),
      round((cursor.y.as_f32() - self.viewport_origin.y) / self.board_zoom(), 2),
    ];
    let Some(session) = self.drag_log.as_mut() else {
      return;
    };
    if session.last_key.as_deref() == Some(key.as_str()) {
      return;
    }
    let t_ms = session.started.elapsed().as_millis() as u64;
    session.last_key = Some(key);
    session.samples.push(json!({
      "t_ms": t_ms,
      "cursor": [round(cursor.x.as_f32(), 1), round(cursor.y.as_f32(), 1)],
      "board_pt": board_pt,
      "over": over,
      "intent": intent,
      "valid_landing": valid,
    }));
  }

  fn cell_label(&self, id: CellId) -> String {
    self
      .board()
      .cell(id)
      .map(|(_, cell)| {
        cell
          .summary
          .summary_text
          .lines()
          .next()
          .unwrap_or_default()
          .trim()
          .to_string()
      })
      .filter(|label| !label.is_empty())
      .unwrap_or_else(|| format!("cell:{}", &id.to_string()[..8]))
  }

  fn describe_intent(&self, intent: FlowDropIntent) -> String {
    match intent {
      FlowDropIntent::BeforeSibling(id) => format!("BeforeSibling({})", self.cell_label(id)),
      FlowDropIntent::AfterSibling(id) => format!("AfterSibling({})", self.cell_label(id)),
      FlowDropIntent::FirstChildOf(id) => format!("FirstChildOf({})", self.cell_label(id)),
      FlowDropIntent::LastChildOf(id) => format!("LastChildOf({})", self.cell_label(id)),
      FlowDropIntent::RootInColumn {
        column_index,
        insertion_index,
      } => format!("RootInColumn{{col:{column_index}, idx:{insertion_index}}}"),
    }
  }

  fn landing_is_valid(&self, intent: FlowDropIntent) -> bool {
    self
      .active_sheet
      .zip(self.dragging_cell)
      .is_some_and(|(sheet, dragged)| flowstate_flow::board_ops::preview_move_cell_subtree(self.board(), sheet, dragged, intent).is_some())
  }

  fn sheet_topology_snapshot(&self) -> Vec<Value> {
    let Some(sheet) = self.active_sheet.and_then(|id| self.board().sheet(id)) else {
      return Vec::new();
    };
    let definition = self.board().format.sheet_type(sheet.sheet_type_id);
    sheet
      .cells
      .iter()
      .map(|cell| {
        let column = definition.and_then(|definition| {
          definition
            .columns
            .iter()
            .position(|column| column.id == cell.column_id)
        });
        json!({
          "label": self.cell_label(cell.id),
          "column": column,
          "parent": cell.parent_id.map(|parent| self.cell_label(parent)),
        })
      })
      .collect()
  }
}
