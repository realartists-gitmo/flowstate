use std::{cell::RefCell, rc::Rc};

use gpui::{
  App, AvailableSpace, Bounds, Element, ElementId, Entity, GlobalElementId, InspectorElementId, IntoElement, LayoutId, Pixels, Style, Window, px,
};

use super::*;

pub struct RichTextDocumentElement {
  pub(super) document: Document,
  pub(super) layout: WordElementLayout,
}

impl RichTextDocumentElement {
  pub fn new(document: Document) -> Self {
    Self {
      document,
      layout: WordElementLayout::default(),
    }
  }
}

impl IntoElement for RichTextDocumentElement {
  type Element = Self;

  fn into_element(self) -> Self::Element {
    self
  }
}

#[derive(Clone)]
pub(super) struct VirtualParagraphElement {
  pub(super) editor: Entity<RichTextEditor>,
  pub(super) paragraph_ix: usize,
  pub(super) generation: u64,
  pub(super) layout: WordElementLayout,
}

impl IntoElement for VirtualParagraphElement {
  type Element = Self;

  fn into_element(self) -> Self::Element {
    self
  }
}

#[derive(Clone, Default)]
pub(super) struct WordElementLayout(Rc<RefCell<Option<Rc<LayoutState>>>>);

impl Element for RichTextDocumentElement {
  type RequestLayoutState = ();
  type PrepaintState = ();

  fn id(&self) -> Option<ElementId> {
    None
  }

  fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
    None
  }

  fn request_layout(
    &mut self,
    _id: Option<&GlobalElementId>,
    _inspector_id: Option<&InspectorElementId>,
    window: &mut Window,
    _cx: &mut App,
  ) -> (LayoutId, Self::RequestLayoutState) {
    request_word_layout(self.document.clone(), self.layout.clone(), window)
  }

  fn prepaint(
    &mut self,
    _id: Option<&GlobalElementId>,
    _inspector_id: Option<&InspectorElementId>,
    bounds: Bounds<Pixels>,
    _request_layout: &mut Self::RequestLayoutState,
    _window: &mut Window,
    _cx: &mut App,
  ) {
    if let Some(layout) = self.layout.0.borrow_mut().as_mut() {
      Rc::make_mut(layout).bounds = Some(bounds);
    }
  }

  fn paint(
    &mut self,
    _id: Option<&GlobalElementId>,
    _inspector_id: Option<&InspectorElementId>,
    _bounds: Bounds<Pixels>,
    _request_layout: &mut Self::RequestLayoutState,
    _prepaint: &mut Self::PrepaintState,
    window: &mut Window,
    cx: &mut App,
  ) {
    if let Some(layout) = self.layout.0.borrow().as_ref().cloned() {
      paint_layout(layout.as_ref(), None, false, window, cx);
    }
  }
}

impl Element for VirtualParagraphElement {
  type RequestLayoutState = ();
  type PrepaintState = ();

  fn id(&self) -> Option<ElementId> {
    None
  }

  fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
    None
  }

  fn request_layout(
    &mut self,
    _id: Option<&GlobalElementId>,
    _inspector_id: Option<&InspectorElementId>,
    window: &mut Window,
    _cx: &mut App,
  ) -> (LayoutId, Self::RequestLayoutState) {
    let editor = self.editor.clone();
    let paragraph_ix = self.paragraph_ix;
    let layout_cell = self.layout.clone();
    let layout_id = window.request_measured_layout(Style::default(), move |known, available, window, cx| {
      let width = known
        .width
        .or(match available.width {
          AvailableSpace::Definite(width) => Some(width),
          _ => Some(px(900.0)),
        })
        .unwrap_or(px(900.0));
      let previous_layout = layout_cell.0.borrow().clone();
      let layout = editor.update(cx, |editor, cx| {
        build_single_paragraph_layout(&editor.document, paragraph_ix, width, previous_layout.as_deref(), window, cx)
      });
      let size = layout.size;
      if let Some(paragraph) = layout.paragraphs.first() {
        let key = paragraph.cache_key;
        editor.update(cx, |editor, cx| {
          editor.update_paragraph_height_cache(paragraph_ix, width, key, size.height, cx)
        });
      }
      layout_cell.0.borrow_mut().replace(Rc::new(layout));
      size
    });
    (layout_id, ())
  }

  fn prepaint(
    &mut self,
    _id: Option<&GlobalElementId>,
    _inspector_id: Option<&InspectorElementId>,
    bounds: Bounds<Pixels>,
    _request_layout: &mut Self::RequestLayoutState,
    _window: &mut Window,
    cx: &mut App,
  ) {
    let layout = {
      let mut layout_ref = self.layout.0.borrow_mut();
      let Some(layout) = layout_ref.as_mut() else {
        return;
      };
      Rc::make_mut(layout).bounds = Some(bounds);
      layout.clone()
    };
    self.editor.update(cx, |editor, _| {
      editor.store_visible_paragraph_layout(self.generation, self.paragraph_ix, layout.as_ref(), bounds);
    });
  }

  fn paint(
    &mut self,
    _id: Option<&GlobalElementId>,
    _inspector_id: Option<&InspectorElementId>,
    _bounds: Bounds<Pixels>,
    _request_layout: &mut Self::RequestLayoutState,
    _prepaint: &mut Self::PrepaintState,
    window: &mut Window,
    cx: &mut App,
  ) {
    let (selection, show_caret) = {
      let editor = self.editor.read(cx);
      (
        editor.selection.clone(),
        editor.selection.is_caret()
          && editor.selection.head.paragraph == self.paragraph_ix
          && editor.caret_visible
          && editor.focus_handle.is_focused(window),
      )
    };
    if let Some(layout) = self.layout.0.borrow().as_ref().cloned() {
      paint_layout(layout.as_ref(), Some(&selection), show_caret, window, cx);
    }
  }
}

pub(super) fn request_word_layout(document: Document, layout_cell: WordElementLayout, window: &mut Window) -> (LayoutId, ()) {
  let layout_id = window.request_measured_layout(Style::default(), move |known, available, window, cx| {
    let width = known
      .width
      .or(match available.width {
        AvailableSpace::Definite(width) => Some(width),
        _ => Some(px(900.0)),
      })
      .unwrap_or(px(900.0));
    let previous_layout = layout_cell.0.borrow().clone();
    let layout = build_layout(&document, width, previous_layout.as_deref(), window, cx);
    let size = layout.size;
    layout_cell.0.borrow_mut().replace(Rc::new(layout));
    size
  });
  (layout_id, ())
}

// -------- Edit / movement helper free functions ------------------------
