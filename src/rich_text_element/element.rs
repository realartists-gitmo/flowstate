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
pub(super) struct VirtualParagraphChunkElement {
  pub(super) editor: Entity<RichTextEditor>,
  pub(super) item_ix: usize,
  pub(super) paragraph_ix: usize,
  pub(super) chunk_ix: usize,
  pub(super) generation: u64,
  pub(super) layout: WordElementLayout,
}

#[derive(Clone)]
pub(super) struct VirtualBlockElement {
  pub(super) editor: Entity<RichTextEditor>,
  pub(super) block_ix: usize,
  pub(super) layout: WordElementLayout,
}

impl IntoElement for VirtualParagraphChunkElement {
  type Element = Self;

  fn into_element(self) -> Self::Element {
    self
  }
}

impl IntoElement for VirtualBlockElement {
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
      paint_layout(layout.as_ref(), None, None, false, px(1.0), window, cx);
    }
  }
}

impl Element for VirtualParagraphChunkElement {
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
    let chunk_ix = self.chunk_ix;
    let layout_cell = self.layout.clone();
    let layout_id = window.request_measured_layout(Style::default(), move |known, available, window, cx| {
      let width = known
        .width
        .or(match available.width {
          AvailableSpace::Definite(width) => Some(width),
          _ => Some(px(900.0)),
        })
        .unwrap_or(px(900.0));
      let layout = editor.update(cx, |editor, cx| {
        editor.layout_paragraph_chunk_for_element(paragraph_ix, chunk_ix, width, window, cx)
      });
      let Some(layout) = layout else {
        return gpui::size(width, px(1.0));
      };
      let size = layout.size;
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
      editor.store_visible_paragraph_chunk_layout(self.generation, self.item_ix, layout.as_ref(), bounds);
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
    let (selection, drag_selection, caret_offset, caret_width) = {
      let editor = self.editor.read(cx);
      let drag_selection = editor.drag_source_selection();
      (
        editor.selection.clone(),
        drag_selection,
        (editor.selection.is_caret()
          && editor.selected_block.is_none()
          && editor.selection.head.paragraph == self.paragraph_ix
          && editor.caret_visible
          && editor.focus_handle.is_focused(window))
        .then_some(editor.selection.head),
        editor.caret_paint_width(),
      )
    };
    if let Some(layout) = self.layout.0.borrow().as_ref().cloned() {
      let show_caret = caret_offset.is_some_and(|offset| {
        layout.paragraphs.first().is_some_and(|paragraph| {
          if !paragraph.contains_byte(offset.byte) {
            return false;
          }

          // Treat chunk ownership as end-exclusive when deciding which
          // VirtualParagraphChunkElement paints the caret. At a boundary byte,
          // the trailing chunk should win; otherwise both adjacent chunks can
          // claim the same caret position when `contains_byte` is inclusive.
          offset
            .byte
            .checked_add(1)
            .is_some_and(|next_byte| paragraph.contains_byte(next_byte))
        })
      });
      paint_layout(
        layout.as_ref(),
        Some(&selection),
        drag_selection.as_ref(),
        show_caret,
        caret_width,
        window,
        cx,
      );
    }
  }
}

impl Element for VirtualBlockElement {
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
    let block_ix = self.block_ix;
    let layout_cell = self.layout.clone();
    let layout_id = window.request_measured_layout(Style::default(), move |known, available, window, cx| {
      let width = known
        .width
        .or(match available.width {
          AvailableSpace::Definite(width) => Some(width),
          _ => Some(px(900.0)),
        })
        .unwrap_or(px(900.0));
      editor.update(cx, |editor, cx| editor.note_measured_item_width(width, cx));
      let (block, paragraph_after, snap_underline_rules_to_pixels) = editor.update(cx, |editor, cx| {
        (
          layout_structural_block_at(&editor.document, block_ix, width, px(0.0), window, cx),
          editor.document.theme.paragraph_after,
          editor.document.theme.snap_underline_rules_to_pixels,
        )
      });
      let height = block
        .as_ref()
        .map(structural_block_height)
        .unwrap_or(px(1.0))
        + paragraph_after;
      let layout = LayoutState {
        paragraphs: Vec::new(),
        blocks: block.into_iter().collect(),
        paragraph_to_block: Vec::new(),
        block_to_paragraph: vec![None],
        bounds: None,
        size: gpui::size(width, height),
        width,
        snap_underline_rules_to_pixels,
      };
      layout_cell.0.borrow_mut().replace(Rc::new(layout));
      gpui::size(width, height)
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
    let (selected_block, table_cell_caret, text_selected) = {
      let editor = self.editor.read(cx);
      (
        editor.selected_block,
        editor.table_cell_caret_for_paint(window),
        editor.block_is_inside_text_selection(self.block_ix),
      )
    };
    let Some(layout) = self.layout.0.borrow().as_ref().cloned() else {
      return;
    };
    let Some(bounds) = layout.bounds else {
      return;
    };
    for block in &layout.blocks {
      paint_structural_block(block, selected_block, table_cell_caret, text_selected, bounds.origin, window, cx);
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
