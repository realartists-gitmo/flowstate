//! I-S3 (I4-A): the sheet strip — one slim tab row on the board's bottom
//! edge. Click switches, drag reorders (identity-anchored `MoveSheet`),
//! double-click renames inline, "+" creates per sheet type, and peer dots
//! sit on the tab where each teammate actually is. The ribbon sheds its
//! sheet widgets; outline rows stay untouched.

use flowstate_flow::SheetId;
use gpui::{
  App, ClickEvent, Context, Entity, FocusHandle, Focusable, IntoElement, Render, SharedString, Subscription, Window, div, prelude::*, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme as _, Sizable as _};

use crate::flow::FlowEditor;

/// Drag payload for a sheet tab. Dropping on a tab lands the dragged sheet
/// immediately BEFORE it; dropping on the strip's tail lands it at the end.
#[derive(Clone)]
struct SheetTabDrag {
  sheet: SheetId,
  name: SharedString,
}

/// The ghost that rides the cursor during a tab drag.
struct SheetTabDragGhost {
  name: SharedString,
}

impl Render for SheetTabDragGhost {
  fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    div()
      .px_2()
      .py_0p5()
      .rounded(px(4.0))
      .bg(cx.theme().secondary)
      .border_1()
      .border_color(cx.theme().border)
      .text_xs()
      .text_color(cx.theme().foreground)
      .child(self.name.clone())
  }
}

pub struct FlowSheetStrip {
  editor: Entity<FlowEditor>,
  /// The sheet whose tab is in inline-rename mode (dbl-click).
  renaming: Option<SheetId>,
  rename_input: Entity<InputState>,
  focus_handle: FocusHandle,
  _subscriptions: Vec<Subscription>,
}

impl FlowSheetStrip {
  pub fn new(editor: Entity<FlowEditor>, window: &mut Window, cx: &mut Context<Self>) -> Self {
    let rename_input = cx.new(|cx| InputState::new(window, cx).placeholder("Sheet name"));
    let mut subscriptions = Vec::new();
    // The strip mirrors editor state (sheets, active, presences) — re-render
    // whenever the editor notifies.
    subscriptions.push(cx.observe(&editor, |_, _, cx| cx.notify()));
    // I-S1 rename law carried over from the ribbon widget: commit once on
    // Enter/blur; an emptied field abandons the rename instead of writing a
    // nameless sheet.
    let rename_editor = editor.clone();
    subscriptions.push(cx.subscribe_in(
      &rename_input,
      window,
      move |strip: &mut Self, input, event: &InputEvent, _window, cx| {
        if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
          let Some(sheet) = strip.renaming.take() else {
            return;
          };
          let name = input.read(cx).value().trim().to_string();
          let editor = rename_editor.clone();
          if !name.is_empty()
            && editor.read(cx).active_sheet() == Some(sheet)
            && editor.read(cx).active_sheet_name().as_deref() != Some(name.as_str())
          {
            editor.update(cx, |editor, cx| editor.rename_active_sheet(name, cx));
          }
          cx.notify();
        }
      },
    ));
    Self {
      editor,
      renaming: None,
      rename_input,
      focus_handle: cx.focus_handle(),
      _subscriptions: subscriptions,
    }
  }

  fn begin_rename(&mut self, sheet: SheetId, window: &mut Window, cx: &mut Context<Self>) {
    self.editor.update(cx, |editor, cx| editor.activate_sheet(sheet, cx));
    let current = self.editor.read(cx).active_sheet_name().unwrap_or_default();
    self.renaming = Some(sheet);
    self.rename_input.update(cx, |input, cx| {
      input.set_value(current, window, cx);
    });
    self.rename_input.focus_handle(cx).focus(window);
    cx.notify();
  }
}

impl Focusable for FlowSheetStrip {
  fn focus_handle(&self, _: &App) -> FocusHandle {
    self.focus_handle.clone()
  }
}

impl Render for FlowSheetStrip {
  fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    let editor = self.editor.clone();
    let active_sheet = editor.read(cx).active_sheet();
    let sheets: Vec<(SheetId, SharedString)> = editor
      .read(cx)
      .board()
      .sheets
      .iter()
      .map(|sheet| {
        let name: SharedString = if sheet.name.trim().is_empty() {
          "Untitled".into()
        } else {
          sheet.name.clone().into()
        };
        (sheet.id, name)
      })
      .collect();
    #[allow(
      clippy::needless_collect,
      reason = "Release the editor read guard before building elements that clone the editor entity."
    )]
    let sheet_types: Vec<SharedString> = editor
      .read(cx)
      .board()
      .format
      .sheet_types
      .iter()
      .map(|sheet_type| SharedString::from(sheet_type.name.clone()))
      .collect();
    // Peer dots live on the tab their owner is actually on (S11 presence).
    let presence_dots: Vec<(SheetId, u32)> = editor
      .read(cx)
      .external_presences()
      .iter()
      .filter_map(|presence| presence.sheet.map(|sheet| (sheet, presence.color_rgb)))
      .collect();

    div()
      .id("flow-sheet-strip")
      .w_full()
      .h(px(30.0))
      .flex()
      .items_center()
      .gap(px(2.0))
      .px(px(6.0))
      .border_t_1()
      .border_color(cx.theme().border)
      .bg(cx.theme().secondary)
      .overflow_x_scroll()
      // Tail drop: land the dragged sheet at the END.
      .on_drop(cx.listener(|strip, drag: &SheetTabDrag, _, cx| {
        let sheet = drag.sheet;
        strip
          .editor
          .update(cx, |editor, cx| editor.move_sheet_before(sheet, None, cx));
      }))
      .children(sheets.iter().enumerate().map(|(index, (sheet_id, name))| {
        let sheet_id = *sheet_id;
        let is_active = active_sheet == Some(sheet_id);
        let renaming = self.renaming == Some(sheet_id);
        let dots: Vec<_> = presence_dots
          .iter()
          .filter(|(dot_sheet, _)| *dot_sheet == sheet_id)
          .map(|(_, color_rgb)| {
            div()
              .size(px(6.0))
              .flex_none()
              .rounded_full()
              .bg(gpui::Hsla::from(gpui::rgba((color_rgb << 8) | 0xff)))
              .into_any_element()
          })
          .collect();

        let mut tab = div()
          .id(("flow-sheet-tab", index))
          .h(px(24.0))
          .px_2()
          .flex()
          .flex_none()
          .items_center()
          .gap(px(4.0))
          .rounded(px(4.0))
          .cursor_pointer()
          .text_xs();
        tab = if is_active {
          tab
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .border_1()
            .border_color(cx.theme().primary)
        } else {
          tab
            .text_color(cx.theme().muted_foreground)
            .hover(|style| style.bg(cx.theme().background.opacity(0.6)))
            .border_1()
            .border_color(gpui::transparent_black())
        };

        if renaming {
          tab = tab.child(div().w(px(130.0)).child(Input::new(&self.rename_input).xsmall().w_full()));
        } else {
          tab = tab.child(name.clone());
        }
        tab = tab.children(dots);
        if is_active && !renaming {
          // The delete verb lives on the active tab; the editor owns the
          // confirmation prompt (a whole sheet of cells goes with it).
          let delete_editor = self.editor.clone();
          tab = tab.child(
            div()
              .id(("flow-sheet-tab-delete", index))
              .flex_none()
              .px(px(2.0))
              .rounded(px(3.0))
              .text_color(cx.theme().muted_foreground)
              .hover(|style| style.text_color(cx.theme().danger))
              .on_click(move |_, window, cx| {
                cx.stop_propagation();
                delete_editor.update(cx, |editor, cx| editor.confirm_delete_active_sheet(window, cx));
              })
              .child("×"),
          );
        }

        let drag_payload = SheetTabDrag {
          sheet: sheet_id,
          name: name.clone(),
        };
        tab
          .on_click(cx.listener(move |strip, event: &ClickEvent, window, cx| {
            if event.click_count() >= 2 {
              strip.begin_rename(sheet_id, window, cx);
            } else {
              strip
                .editor
                .update(cx, |editor, cx| editor.activate_sheet(sheet_id, cx));
            }
          }))
          .on_drag(drag_payload, |drag, _, _, cx| {
            let name = drag.name.clone();
            cx.new(|_| SheetTabDragGhost { name })
          })
          .drag_over::<SheetTabDrag>(|style, _, _, cx| style.bg(cx.theme().primary.opacity(0.18)))
          .on_drop(cx.listener(move |strip, drag: &SheetTabDrag, _, cx| {
            cx.stop_propagation();
            let dragged = drag.sheet;
            strip
              .editor
              .update(cx, |editor, cx| editor.move_sheet_before(dragged, Some(sheet_id), cx));
          }))
          .into_any_element()
      }))
      // Typed create: one "+" per sheet type (the format defines the types).
      .children(sheet_types.into_iter().enumerate().map(|(index, name)| {
        let editor = editor.clone();
        div()
          .id(("flow-sheet-strip-create", index))
          .h(px(22.0))
          .px_2()
          .flex()
          .flex_none()
          .items_center()
          .rounded(px(4.0))
          .cursor_pointer()
          .text_xs()
          .text_color(cx.theme().muted_foreground)
          .hover(|style| style.bg(cx.theme().background.opacity(0.6)).text_color(cx.theme().foreground))
          .on_click(move |_, _, cx| editor.update(cx, |editor, cx| editor.create_sheet_of_type(index, cx)))
          .child(format!("+ {name}"))
      }))
  }
}
