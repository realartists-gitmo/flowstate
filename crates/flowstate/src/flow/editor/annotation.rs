use flowstate_flow::{AnnotationOriginator, AnnotationStroke, BoardPoint, BoardRect, StrokeStyle};
use gpui::{Context, Hsla, PathBuilder, Pixels, Point, Window, point, px};
use gpui_component::PixelsExt as _;

use super::{AnnotationTool, FlowEditor};

impl FlowEditor {
  pub fn marker_color_rgba(&self) -> u32 {
    self.marker_color_rgba
  }

  /// I-S2: pick a pen color (and arm the marker — picking a pen means you
  /// want to draw).
  pub fn set_marker_color(&mut self, color_rgba: u32, cx: &mut Context<Self>) {
    self.marker_color_rgba = color_rgba;
    self.annotation_tool = AnnotationTool::Marker;
    cx.notify();
  }

  pub fn set_annotation_tool(&mut self, tool: AnnotationTool, cx: &mut Context<Self>) {
    self.annotation_tool = tool;
    cx.notify();
  }

  pub fn toggle_annotation_tool(&mut self, tool: AnnotationTool, cx: &mut Context<Self>) {
    self.annotation_tool = if self.annotation_tool == tool { AnnotationTool::None } else { tool };
    cx.notify();
  }

  pub fn toggle_annotations_visible(&mut self, cx: &mut Context<Self>) {
    let Some(sheet) = self.active_sheet else {
      return;
    };
    if !self.hidden_annotation_sheets.remove(&sheet) {
      self.hidden_annotation_sheets.insert(sheet);
    }
    cx.notify();
  }

  pub fn set_originator_annotations_hidden(&mut self, originator: AnnotationOriginator, hidden: bool, cx: &mut Context<Self>) {
    if hidden {
      self.hidden_annotation_originators.insert(originator);
    } else {
      self.hidden_annotation_originators.remove(&originator);
    }
    cx.notify();
  }

  pub fn set_local_annotation_originator(&mut self, originator: AnnotationOriginator) {
    self.local_annotation_originator = originator;
  }

  /// I-S1: the ribbon's "Clear ink" clears THIS SHEET — exactly what its label
  /// always claimed (the old implementation wiped every sheet). Clears both
  /// the local identity's strokes and legacy pre-identity "local" strokes.
  pub fn clear_annotations(&mut self, cx: &mut Context<Self>) {
    let Some(sheet) = self.active_sheet else {
      return;
    };
    self.clear_annotations_in_scope(flowstate_flow::AnnotationScope::Sheet(sheet), cx);
  }

  /// I-S1: the explicit every-sheet clear — a separate, confirmed verb.
  pub fn clear_all_annotations(&mut self, cx: &mut Context<Self>) {
    self.clear_annotations_in_scope(flowstate_flow::AnnotationScope::AllSheets, cx);
  }

  fn clear_annotations_in_scope(&mut self, scope: flowstate_flow::AnnotationScope, cx: &mut Context<Self>) {
    let mut cleared = false;
    for originator in self.erasable_originators() {
      let intent = flowstate_flow::FlowIntent::ClearAnnotations {
        scope: scope.clone(),
        originator,
      };
      cleared |= self.apply_intent(&intent, cx).is_ok();
    }
    if cleared {
      self.changed(self.active_cell, cx);
    }
  }

  /// I-S1 ownership rule: you erase YOUR strokes — plus legacy `"local"`
  /// strokes, which carry zero identity information (every pre-identity peer
  /// stamped that literal; write-once blobs can't be adopted, and freezing
  /// them would make legacy ink immortal).
  fn erasable_originators(&self) -> Vec<AnnotationOriginator> {
    let legacy = AnnotationOriginator("local".into());
    if self.local_annotation_originator == legacy {
      vec![legacy]
    } else {
      vec![self.local_annotation_originator.clone(), legacy]
    }
  }

  fn originator_erasable(&self, originator: &AnnotationOriginator) -> bool {
    originator == &self.local_annotation_originator || originator.0 == "local"
  }

  fn annotation_point(&self, position: Point<Pixels>) -> BoardPoint {
    BoardPoint {
      x: (position.x.as_f32() - self.viewport_origin.x) / self.board_zoom,
      y: (position.y.as_f32() - self.viewport_origin.y) / self.board_zoom,
    }
  }

  pub(super) fn set_viewport_origin(&mut self, origin: Point<Pixels>) {
    self.viewport_origin = BoardPoint {
      x: origin.x.as_f32(),
      y: origin.y.as_f32(),
    };
  }

  pub(super) fn begin_annotation(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
    match self.annotation_tool {
      AnnotationTool::Marker => {
        self.drawing_points.clear();
        self.drawing_points.push(self.annotation_point(position));
      },
      AnnotationTool::Eraser => self.erase_at(self.annotation_point(position), cx),
      AnnotationTool::None => {},
    }
    cx.notify();
  }

  pub(super) fn continue_annotation(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
    if self.annotation_tool == AnnotationTool::Eraser {
      self.erase_at(self.annotation_point(position), cx);
      return;
    }
    if self.annotation_tool != AnnotationTool::Marker || self.drawing_points.is_empty() {
      return;
    }
    let point = self.annotation_point(position);
    let should_append = self.drawing_points.last().is_none_or(|last| {
      let dx = point.x - last.x;
      let dy = point.y - last.y;
      dx * dx + dy * dy >= 4.0
    });
    if should_append {
      self.drawing_points.push(point);
      cx.notify();
    }
  }

  pub(super) fn finish_annotation(&mut self, cx: &mut Context<Self>) {
    let Some(sheet_id) = self.active_sheet else {
      self.drawing_points.clear();
      return;
    };
    if self.drawing_points.len() < 2 {
      self.drawing_points.clear();
      return;
    }
    let points = simplify_stroke(&std::mem::take(&mut self.drawing_points), 1.5);
    let bbox = stroke_bbox(&points);
    let stroke = AnnotationStroke {
      id: uuid::Uuid::new_v4(),
      sheet_id,
      originator: self.local_annotation_originator.clone(),
      points,
      style: StrokeStyle {
        // I-S2: the chosen pen color (color_rgba was in every stroke blob all
        // along — only the hardcoded amber ever reached it).
        color_rgba: self.marker_color_rgba,
        width: 4.0,
        opacity: 0.55,
      },
      bbox,
    };
    if self
      .apply_intent(&flowstate_flow::FlowIntent::AddAnnotation { stroke }, cx)
      .is_ok()
    {
      self.changed(self.active_cell, cx);
    }
  }

  fn erase_at(&mut self, point: BoardPoint, cx: &mut Context<Self>) {
    let Some(sheet) = self
      .active_sheet
      .and_then(|sheet_id| self.board.sheets.iter().find(|sheet| sheet.id == sheet_id))
    else {
      return;
    };
    let radius = 10.0;
    let touched = sheet
      .annotations
      .iter()
      .find(|stroke| {
        self.originator_erasable(&stroke.originator)
          && point.x >= stroke.bbox.min.x - radius
          && point.x <= stroke.bbox.max.x + radius
          && point.y >= stroke.bbox.min.y - radius
          && point.y <= stroke.bbox.max.y + radius
          && stroke
            .points
            .windows(2)
            .any(|segment| segment_distance(point, segment[0], segment[1]) <= radius)
      })
      // I-S1: the delete intent carries the STROKE's originator so the
      // executor's ownership check passes for legacy "local" strokes too.
      .map(|stroke| (stroke.id, stroke.originator.clone()));
    let sheet_id = sheet.id;
    if let Some((stroke_id, originator)) = touched
      && self
        .apply_intent(
          &flowstate_flow::FlowIntent::DeleteAnnotation {
            sheet_id,
            stroke_id,
            originator,
          },
          cx,
        )
        .is_ok()
    {
      self.changed(self.active_cell, cx);
    }
  }
}

fn stroke_bbox(points: &[BoardPoint]) -> BoardRect {
  let mut min = points[0];
  let mut max = points[0];
  for point in &points[1..] {
    min.x = min.x.min(point.x);
    min.y = min.y.min(point.y);
    max.x = max.x.max(point.x);
    max.y = max.y.max(point.y);
  }
  BoardRect { min, max }
}

fn simplify_stroke(points: &[BoardPoint], minimum_distance: f32) -> Vec<BoardPoint> {
  let Some(first) = points.first().copied() else {
    return Vec::new();
  };
  let mut simplified = Vec::with_capacity(points.len());
  simplified.push(first);
  for window in points.windows(3) {
    let smoothed = BoardPoint {
      x: (window[0].x + window[1].x + window[2].x) / 3.0,
      y: (window[0].y + window[1].y + window[2].y) / 3.0,
    };
    if simplified
      .last()
      .is_none_or(|previous| (smoothed.x - previous.x).hypot(smoothed.y - previous.y) >= minimum_distance)
    {
      simplified.push(smoothed);
    }
  }
  if let Some(last) = points.last().copied()
    && simplified.last() != Some(&last)
  {
    simplified.push(last);
  }
  simplified
}

fn segment_distance(point: BoardPoint, start: BoardPoint, end: BoardPoint) -> f32 {
  let dx = end.x - start.x;
  let dy = end.y - start.y;
  let length_squared = dx * dx + dy * dy;
  let t = if length_squared == 0.0 {
    0.0
  } else {
    (((point.x - start.x) * dx + (point.y - start.y) * dy) / length_squared).clamp(0.0, 1.0)
  };
  let nearest_x = start.x + t * dx;
  let nearest_y = start.y + t * dy;
  (point.x - nearest_x).hypot(point.y - nearest_y)
}

pub(super) fn paint_stroke(origin: Point<Pixels>, points: &[BoardPoint], width: Pixels, color: Hsla, zoom: f32, window: &mut Window) {
  let Some(first) = points.first() else {
    return;
  };
  let mut path = PathBuilder::stroke(width);
  path.move_to(point(origin.x + px(first.x * zoom), origin.y + px(first.y * zoom)));
  for point_value in &points[1..] {
    path.line_to(point(origin.x + px(point_value.x * zoom), origin.y + px(point_value.y * zoom)));
  }
  if let Ok(path) = path.build() {
    window.paint_path(path, color);
  }
}
