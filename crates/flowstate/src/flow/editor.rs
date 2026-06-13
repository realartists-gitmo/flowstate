use std::path::PathBuf;

use gpui::{App, Context, EventEmitter, FocusHandle, Focusable, IntoElement, Render, Task, Window, div, prelude::*};

pub struct FlowEditor {
  path: Option<PathBuf>,
  dirty: bool,
  focus_handle: FocusHandle,
}

impl FlowEditor {
  pub fn new_with_path(_document: flowstate_flow::FlowDocument, path: Option<PathBuf>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
    Self {
      path,
      dirty: false,
      focus_handle: cx.focus_handle(),
    }
  }

  pub fn blank(window: &mut Window, cx: &mut Context<Self>) -> Self {
    Self::new_with_path(flowstate_flow::FlowDocument::new(), None, window, cx)
  }

  pub fn load_or_new(path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
    Self::new_with_path(flowstate_flow::load_flow_document_or_new(&path), Some(path), window, cx)
  }

  pub fn document_path(&self) -> Option<&PathBuf> {
    self.path.as_ref()
  }

  pub fn set_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
    self.path = Some(path);
    cx.notify();
  }

  pub fn has_unsaved_changes(&self) -> bool {
    self.dirty
  }

  pub fn save(&mut self, cx: &mut Context<Self>) -> Task<std::io::Result<()>> {
    let Some(path) = self.path.clone() else {
      return cx.background_executor().spawn(async {
        Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "flow has no save path"))
      });
    };
    self.save_to_path(path, cx)
  }

  pub fn save_as(&mut self, path: PathBuf, cx: &mut Context<Self>) -> Task<std::io::Result<()>> {
    self.path = Some(path.clone());
    self.save_to_path(path, cx)
  }

  fn save_to_path(&mut self, path: PathBuf, cx: &mut Context<Self>) -> Task<std::io::Result<()>> {
    cx.spawn(async move |editor, cx| {
      let write_result = cx
        .background_executor()
        .spawn(async move { flowstate_flow::save_flow_document(&path, &flowstate_flow::FlowDocument::new()).map_err(std::io::Error::other) })
        .await;
      match write_result {
        Ok(()) => {
          let _ = editor.update(cx, |editor, cx| {
            editor.dirty = false;
            cx.notify();
          });
          Ok(())
        },
        Err(error) => Err(error),
      }
    })
  }

  pub fn discard_recovery_file(&mut self) {}

  pub fn resolve_pending(&mut self, _cx: &mut Context<Self>) {}
}

impl EventEmitter<()> for FlowEditor {}

impl Focusable for FlowEditor {
  fn focus_handle(&self, _: &App) -> FocusHandle {
    self.focus_handle.clone()
  }
}

impl Render for FlowEditor {
  fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
    div().size_full()
  }
}
