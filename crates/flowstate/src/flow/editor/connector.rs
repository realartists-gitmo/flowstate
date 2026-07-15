use gpui::{Bounds, Hsla, PathBuilder, Pixels, Point, Window, point, px};
use gpui_component::PixelsExt as _;

#[derive(Debug, PartialEq)]
struct ConnectorGeometry {
  start: Point<Pixels>,
  midpoint_x: Pixels,
  children: Vec<Point<Pixels>>,
}

/// Part 4 wires: tapered two-pass stroke — the parent half carries the
/// thicker gauge (2.2px), the answer half thins to 1px; `emphasis` is the
/// hover/focus thickening multiplier (spec G5: ~2.5px accent when focused).
pub(super) fn paint_connector_family(parent: Bounds<Pixels>, children: &[Bounds<Pixels>], color: Hsla, emphasis: f32, window: &mut Window) {
  let Some(geometry) = connector_geometry(parent, children, window.scale_factor()) else {
    return;
  };
  // Pass 1 (thick): parent stub + trunk.
  let mut trunk = PathBuilder::stroke(px(2.2 * emphasis));
  trunk.move_to(geometry.start);
  trunk.line_to(point(geometry.midpoint_x, geometry.start.y));
  let min_y = geometry
    .children
    .iter()
    .map(|child| child.y)
    .fold(geometry.start.y, Pixels::min);
  let max_y = geometry
    .children
    .iter()
    .map(|child| child.y)
    .fold(geometry.start.y, Pixels::max);
  if min_y != max_y {
    trunk.move_to(point(geometry.midpoint_x, min_y));
    trunk.line_to(point(geometry.midpoint_x, max_y));
  }
  if let Ok(path) = trunk.build() {
    window.paint_path(path, color);
  }
  // Pass 2 (thin): the answer-side branches.
  let mut branches = PathBuilder::stroke(px(1.0 * emphasis));
  let mut any = false;
  for child in geometry.children {
    branches.move_to(point(geometry.midpoint_x, child.y));
    branches.line_to(child);
    any = true;
  }
  if any && let Ok(path) = branches.build() {
    window.paint_path(path, color);
  }
  // G5: the fat plug at the parent end — the future re-plug drag handle
  // grows with emphasis so a focused wire advertises its grip.
  let plug = px(2.6 * emphasis);
  window.paint_quad(gpui::quad(
    Bounds::centered_at(geometry.start, gpui::size(plug * 2.0, plug * 2.0)),
    plug,
    color,
    px(0.0),
    color,
    gpui::BorderStyle::default(),
  ));
}

fn connector_geometry(parent: Bounds<Pixels>, children: &[Bounds<Pixels>], device_scale: f32) -> Option<ConnectorGeometry> {
  let first_child = children.first()?;
  let snap = |value: Pixels| px(((value.as_f32() * device_scale).floor() + 0.5) / device_scale);
  let start = point(snap(parent.right()), snap(parent.center().y));
  let midpoint_x = snap(start.x + (first_child.left() - start.x) / 2.0);
  let children = children
    .iter()
    .map(|child| point(snap(child.left()), snap(child.center().y)))
    .collect();
  Some(ConnectorGeometry { start, midpoint_x, children })
}

#[cfg(test)]
mod tests {
  use super::*;
  use gpui::{Bounds, size};

  #[test]
  fn connector_targets_each_child_midpoint() {
    let parent = Bounds::new(point(px(0.0), px(10.0)), size(px(100.0), px(40.0)));
    let children = [
      Bounds::new(point(px(200.0), px(10.0)), size(px(100.0), px(80.0))),
      Bounds::new(point(px(200.0), px(110.0)), size(px(100.0), px(40.0))),
    ];

    let geometry = connector_geometry(parent, &children, 1.0).unwrap();

    assert_eq!(geometry.start.y, px(30.5));
    assert_eq!(geometry.children[0].y, px(50.5));
    assert_eq!(geometry.children[1].y, px(130.5));
  }
}
