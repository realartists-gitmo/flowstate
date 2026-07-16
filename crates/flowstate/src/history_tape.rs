//! The history TAPE — Adam's decided instrument: one continuous timeline
//! track with a little draggable toggle (the thumb) and marks at the notable
//! points (checkpoints: named pins tall and solid, session saves medium,
//! autosave grain small and ghosted). Scrolls horizontally when history gets
//! dense. Shared by the .db8 takeover and the flow editor's history mode —
//! one instrument, both formats.

use std::cell::Cell;
use std::rc::Rc;

use flowstate_document::RevisionKind;
use gpui::{AnyElement, App, Bounds, MouseButton, Pixels, SharedString, Window, canvas, div, prelude::*, px, relative};
use gpui_component::{ActiveTheme as _, scroll::ScrollableElement as _};

/// A notable point on the tape.
#[derive(Clone)]
pub struct TapeMark {
  pub id: u128,
  /// 0.0..=1.0 along the timeline.
  pub position: f32,
  pub kind: RevisionKind,
  pub title: SharedString,
}

/// Marks per tape width before the track grows and scrolls.
const DENSE_MARK_SPACING_PX: f32 = 18.0;
const TRACK_MIN_WIDTH_PX: f32 = 240.0;

type ScrubHandler = Rc<dyn Fn(f32, &mut Window, &mut App)>;
type MarkHandler = Rc<dyn Fn(u128, &mut Window, &mut App)>;

/// Build the tape element. `bounds_slot` is host-owned storage the track
/// writes its painted bounds into (drag math reads it back); `on_scrub`
/// receives continuous 0..=1 positions from clicks and drags; `on_mark`
/// receives exact mark hits (which beat scrubbing — a mark is a landmark).
pub fn history_tape(
  id: &'static str,
  position: f32,
  marks: Vec<TapeMark>,
  selected_mark: Option<u128>,
  bounds_slot: Rc<Cell<Option<Bounds<Pixels>>>>,
  on_scrub: ScrubHandler,
  on_mark: MarkHandler,
  cx: &App,
) -> AnyElement {
  let position = position.clamp(0.0, 1.0);
  // Density: the track grows past the viewport when marks crowd, and the
  // strip scrolls — the timeline never crushes its landmarks together.
  let track_min_width = px((marks.len() as f32 * DENSE_MARK_SPACING_PX).max(TRACK_MIN_WIDTH_PX));
  let scrub_down = on_scrub.clone();
  let bounds_for_canvas = bounds_slot.clone();
  let bounds_for_down = bounds_slot.clone();
  let bounds_for_move = bounds_slot;

  div()
    .id((id, 0usize))
    .flex_1()
    .min_w_0()
    .h(px(34.0))
    .overflow_x_scrollbar()
    .child(
      div()
        .id((id, 1usize))
        .h_full()
        .w_full()
        .min_w(track_min_width)
        .relative()
        .flex()
        .items_center()
        // The track.
        .child(
          div()
            .absolute()
            .left_0()
            .right_0()
            .h(px(6.0))
            .rounded_full()
            .bg(cx.theme().border.opacity(0.6)),
        )
        // The elapsed portion.
        .child(
          div()
            .absolute()
            .left_0()
            .w(relative(position))
            .h(px(6.0))
            .rounded_full()
            .bg(cx.theme().primary.opacity(0.45)),
        )
        // Marks — the notable points. Painted before the thumb so the toggle
        // rides above them.
        .children(marks.into_iter().enumerate().map(|(ix, mark)| {
          let (height, width, color) = match mark.kind {
            RevisionKind::Named => (px(18.0), px(5.0), cx.theme().warning),
            RevisionKind::Session => (px(13.0), px(3.0), cx.theme().link),
            RevisionKind::Auto => (px(8.0), px(3.0), cx.theme().muted_foreground.opacity(0.5)),
          };
          let selected = selected_mark == Some(mark.id);
          let on_mark = on_mark.clone();
          let mark_id = mark.id;
          let tooltip = mark.title.clone();
          div()
            .id((id, 100usize + ix))
            .absolute()
            .left(relative(mark.position.clamp(0.0, 1.0)))
            .top_0()
            .bottom_0()
            // A forgiving hit area around the visible mark.
            .w(px(12.0))
            .ml(px(-6.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx))
            .on_mouse_down(MouseButton::Left, {
              move |_, window, cx| {
                cx.stop_propagation();
                on_mark(mark_id, window, cx);
              }
            })
            .child(
              div()
                .w(width)
                .h(height)
                .rounded_full()
                .bg(color)
                .when(selected, |this| {
                  this.border_1().border_color(cx.theme().foreground)
                }),
            )
            .into_any_element()
        }))
        // The little toggle.
        .child(
          div()
            .absolute()
            .left(relative(position))
            .ml(px(-7.0))
            .w(px(14.0))
            .h(px(14.0))
            .rounded_full()
            .bg(cx.theme().primary)
            .border_2()
            .border_color(cx.theme().background)
            .shadow_sm(),
        )
        // Bounds capture for the drag math.
        .child(
          canvas(
            move |bounds, _, _| bounds_for_canvas.set(Some(bounds)),
            |_, _, _, _| {},
          )
          .absolute()
          .size_full(),
        )
        .on_mouse_down(MouseButton::Left, {
          move |event: &gpui::MouseDownEvent, window, cx| {
            if let Some(fraction) = fraction_at(&bounds_for_down, event.position.x) {
              scrub_down(fraction, window, cx);
            }
          }
        })
        .on_mouse_move({
          move |event: &gpui::MouseMoveEvent, window, cx| {
            if event.pressed_button == Some(MouseButton::Left)
              && let Some(fraction) = fraction_at(&bounds_for_move, event.position.x)
            {
              on_scrub(fraction, window, cx);
            }
          }
        }),
    )
    .into_any_element()
}

fn fraction_at(bounds_slot: &Rc<Cell<Option<Bounds<Pixels>>>>, x: Pixels) -> Option<f32> {
  let bounds = bounds_slot.get()?;
  if bounds.size.width <= px(1.0) {
    return None;
  }
  Some((f32::from(x - bounds.left()) / f32::from(bounds.size.width)).clamp(0.0, 1.0))
}

/// Snap helper for hosts whose checkouts are mark-exact (the .db8 takeover
/// until arbitrary-frontier scrub lands): the nearest mark to a scrub.
#[must_use]
pub fn nearest_mark(marks: &[TapeMark], fraction: f32) -> Option<u128> {
  marks
    .iter()
    .min_by(|a, b| {
      (a.position - fraction)
        .abs()
        .partial_cmp(&(b.position - fraction).abs())
        .unwrap_or(std::cmp::Ordering::Equal)
    })
    .map(|mark| mark.id)
}
