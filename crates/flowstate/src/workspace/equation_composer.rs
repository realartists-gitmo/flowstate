//! B-S8 (B5-A): the equation composer — an anchored popover with a REAL
//! LaTeX input (layout, hit-testing, IME), a live render above it, and
//! inline parse errors. Enter commits, Escape/outside-click cancels. The old
//! in-document source strip and its fake per-character hit-testing are gone;
//! this is the only equation editing surface.

use gpui::{
  Bounds, Context, Entity, EventEmitter, Focusable as _, IntoElement, MouseButton, Pixels, Render, Subscription, WeakEntity, Window,
  anchored, deferred, div, point, prelude::*, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::ActiveTheme as _;

use crate::rich_text_element::RichTextEditor;

#[derive(Clone, Copy, Debug)]
pub enum EquationComposerEvent {
  Dismissed,
}

pub struct EquationComposer {
  editor: WeakEntity<RichTextEditor>,
  /// `Some` = editing an existing equation block; `None` = composing a new
  /// one to insert at the caret.
  equation: Option<crate::rich_text_element::BlockId>,
  input: Entity<InputState>,
  /// Window-space frame of the existing equation (the popover anchor).
  anchor: Option<Bounds<Pixels>>,
  _subscriptions: Vec<Subscription>,
}

impl EquationComposer {
  pub fn new(
    editor: WeakEntity<RichTextEditor>,
    equation: Option<crate::rich_text_element::BlockId>,
    source: &str,
    anchor: Option<Bounds<Pixels>>,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> Self {
    let source = source.to_string();
    let input = cx.new(|cx| {
      let mut state = InputState::new(window, cx).placeholder(r"LaTeX, e.g. \frac{a}{b} = c");
      state.set_value(source, window, cx);
      state
    });
    input.read(cx).focus_handle(cx).focus(window);
    let subscription = cx.subscribe_in(&input, window, |composer: &mut Self, _, event: &InputEvent, window, cx| match event {
      InputEvent::Change => cx.notify(),
      InputEvent::PressEnter { .. } => composer.commit(window, cx),
      _ => {},
    });
    Self {
      editor,
      equation,
      input,
      anchor,
      _subscriptions: vec![subscription],
    }
  }

  /// Headless-test seam: overwrite the input's source.
  #[cfg_attr(not(test), allow(dead_code, reason = "headless-test seam"))]
  pub(crate) fn set_source(&mut self, source: &str, window: &mut Window, cx: &mut Context<Self>) {
    let source = source.to_string();
    self.input.update(cx, |input, cx| input.set_value(source, window, cx));
    cx.notify();
  }

  /// The block this composer edits (`None` = compose-new).
  #[cfg_attr(not(test), allow(dead_code, reason = "headless-test seam"))]
  pub(crate) fn target_equation(&self) -> Option<crate::rich_text_element::BlockId> {
    self.equation
  }

  pub(crate) fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let source = self.input.read(cx).value().trim().to_string();
    if let Some(editor) = self.editor.upgrade() {
      if !source.is_empty() {
        editor.update(cx, |editor, cx| match self.equation {
          Some(equation) => {
            editor.replace_equation_source(equation, &source, cx);
          },
          None => editor.insert_equation(source.clone(), cx),
        });
      }
      editor.read(cx).focus_handle(cx).focus(window);
    }
    cx.emit(EquationComposerEvent::Dismissed);
  }

  fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    if let Some(editor) = self.editor.upgrade() {
      editor.read(cx).focus_handle(cx).focus(window);
    }
    cx.emit(EquationComposerEvent::Dismissed);
  }
}

impl EventEmitter<EquationComposerEvent> for EquationComposer {}

impl Render for EquationComposer {
  fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    let source = self.input.read(cx).value().to_string();
    let preview = if source.trim().is_empty() {
      None
    } else {
      Some(crate::rich_text_element::equation_preview_image(source.trim(), true))
    };

    let window_size = window.bounds().size;
    let card_width = px(420.0);
    // Anchor just below the equation's frame; compose-new floats near the
    // top-center of the window (the caret's neighborhood on most screens).
    let position = match self.anchor {
      Some(anchor) => point(
        anchor
          .left()
          .min(window_size.width - card_width - px(16.0))
          .max(px(8.0)),
        anchor.bottom() + px(6.0),
      ),
      None => point((window_size.width - card_width) / 2.0, px(140.0)),
    };

    let card = div()
      .w(card_width)
      .flex()
      .flex_col()
      .gap_2()
      .p_3()
      .rounded(px(8.0))
      .bg(cx.theme().background)
      .border_1()
      .border_color(cx.theme().border)
      .shadow_lg()
      .child(match preview {
        // Live render: the same cached pipeline the document paints with.
        Some(Ok((image, (width, height)))) => {
          let scale = (f32::from(card_width - px(24.0)) / width.max(1.0)).min(1.0);
          div()
            .w_full()
            .flex()
            .items_center()
            .justify_center()
            .py_1()
            .child(
              gpui::img(image)
                .w(px(width * scale))
                .h(px(height * scale))
                .object_fit(gpui::ObjectFit::ScaleDown),
            )
            .into_any_element()
        },
        // Inline diagnostic — a broken pattern must never read as "nothing".
        Some(Err(error)) => div()
          .w_full()
          .text_xs()
          .text_color(cx.theme().danger)
          .child(error)
          .into_any_element(),
        None => div()
          .w_full()
          .text_xs()
          .text_color(cx.theme().muted_foreground)
          .child("Type LaTeX to preview")
          .into_any_element(),
      })
      .child(Input::new(&self.input).w_full())
      .child(
        div()
          .w_full()
          .text_xs()
          .text_color(cx.theme().muted_foreground)
          .child(if self.equation.is_some() {
            "Enter updates the equation · Esc cancels"
          } else {
            "Enter inserts the equation · Esc cancels"
          }),
      );

    // Occluding backdrop: outside click cancels (and Escape blurs through
    // the same path via the input's cancel handling).
    let backdrop = div()
      .w(window_size.width)
      .h(window_size.height)
      .occlude()
      .on_mouse_down(
        MouseButton::Left,
        cx.listener(|composer, _, window, cx| composer.dismiss(window, cx)),
      )
      .on_key_down(cx.listener(|composer, event: &gpui::KeyDownEvent, window, cx| {
        if event.keystroke.key == "escape" {
          composer.dismiss(window, cx);
        }
      }))
      .child(div().absolute().left(position.x).top(position.y).child(card));

    deferred(anchored().child(backdrop)).with_priority(60)
  }
}
