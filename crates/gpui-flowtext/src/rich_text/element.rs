use std::{cell::RefCell, rc::Rc};

use gpui::{
  App, AvailableSpace, Bounds, Element, ElementId, Entity, GlobalElementId, InspectorElementId, IntoElement, LayoutId, Pixels, Style, Window,
  px, relative,
};

use super::*;

pub struct RichTextDocumentElement {
  pub(super) document: Document,
  pub(super) layout: WordElementLayout,
  pub(super) invisibility_mode: bool,
}

#[hotpath::measure_all]
impl RichTextDocumentElement {
  pub fn new(document: Document) -> Self {
    Self {
      document,
      layout: WordElementLayout::default(),
      invisibility_mode: false,
    }
  }

  #[must_use]
  pub fn with_invisibility_mode(mut self, enabled: bool) -> Self {
    self.invisibility_mode = enabled;
    self
  }
}

#[hotpath::measure_all]
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

#[derive(Clone)]
pub(super) struct EmptyVirtualItemElement;

#[hotpath::measure_all]
impl IntoElement for VirtualParagraphChunkElement {
  type Element = Self;

  fn into_element(self) -> Self::Element {
    self
  }
}

#[hotpath::measure_all]
impl IntoElement for VirtualBlockElement {
  type Element = Self;

  fn into_element(self) -> Self::Element {
    self
  }
}

#[hotpath::measure_all]
impl IntoElement for EmptyVirtualItemElement {
  type Element = Self;

  fn into_element(self) -> Self::Element {
    self
  }
}

#[derive(Clone, Default)]
pub(super) struct WordElementLayout(Rc<RefCell<WordElementLayoutState>>);

#[derive(Default)]
struct WordElementLayoutState {
  layout: Option<Rc<LayoutState>>,
  bounds: Option<Bounds<Pixels>>,
}

#[hotpath::measure_all]
impl WordElementLayout {
  fn set_layout(&self, layout: Rc<LayoutState>) {
    self.0.borrow_mut().layout = Some(layout);
  }

  fn set_bounds(&self, bounds: Bounds<Pixels>) {
    self.0.borrow_mut().bounds = Some(bounds);
  }

  fn layout(&self) -> Option<Rc<LayoutState>> {
    self.0.borrow().layout.clone()
  }

  fn positioned(&self) -> Option<(Rc<LayoutState>, Bounds<Pixels>)> {
    let state = self.0.borrow();
    Some((state.layout.as_ref()?.clone(), state.bounds?))
  }
}

#[hotpath::measure_all]
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
    request_word_layout(self.document.clone(), self.layout.clone(), self.invisibility_mode, window)
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
    self.layout.set_bounds(bounds);
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
    if let Some((layout, bounds)) = self.layout.positioned() {
      paint_layout(layout.as_ref(), bounds, None, None, false, px(1.0), None, &[], &[], window, cx);
    }
  }
}

#[hotpath::measure_all]
impl Element for VirtualParagraphChunkElement {
  type RequestLayoutState = ();
  type PrepaintState = ();

  fn id(&self) -> Option<ElementId> {
    Some(paragraph_chunk_element_id(self.paragraph_ix, self.chunk_ix))
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
      layout_cell.set_layout(layout);
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
    self.layout.set_bounds(bounds);
    let Some(layout) = self.layout.layout() else {
      return;
    };
    self.editor.update(cx, |editor, _| {
      editor.store_visible_paragraph_chunk_layout(self.generation, self.item_ix, self.chunk_ix, layout.as_ref(), bounds);
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
    let paragraph_ix = self.paragraph_ix;
    let (selection, drag_selection, caret_offset, caret_width, local_caret_color_rgb, external_carets, external_selections) = {
      let editor = self.editor.read(cx);
      let drag_selection = editor.drag_source_selection();
      let external_carets = editor.external_carets_for_paragraph(paragraph_ix);
      let external_selections = editor.external_selections_for_paragraph(paragraph_ix);
      let caret_offset = editor.selection.head;
      (
        editor.selection.clone(),
        drag_selection,
        (editor.selection.is_caret()
          && editor.selected_block.is_none()
          && caret_offset.paragraph == paragraph_ix
          && editor.caret_visible
          && editor.focus_handle.is_focused(window))
        .then_some(caret_offset),
        editor.caret_paint_width(),
        editor.local_caret_color_rgb,
        external_carets,
        external_selections,
      )
    };
    if let Some((layout, bounds)) = self.layout.positioned() {
      let show_caret = caret_offset.is_some_and(|offset| {
        layout.paragraphs.first().is_some_and(|paragraph| {
          if !paragraph.contains_byte(offset.byte) {
            return false;
          }

          // Treat chunk ownership as end-exclusive at chunk boundaries so the
          // trailing chunk paints the caret. The paragraph end is the one
          // exception: there is no trailing byte, so the final chunk owns it.
          offset.byte == paragraph.len
            || offset
              .byte
              .checked_add(1)
              .is_some_and(|next_byte| paragraph.contains_byte(next_byte))
        })
      });
      let external_carets = external_carets
        .into_iter()
        .filter(|caret| caret_offset_belongs_to_chunk(layout.as_ref(), caret.offset))
        .collect::<Vec<_>>();
      let external_selections = external_selections
        .into_iter()
        .filter(|selection| selection_belongs_to_chunk(layout.as_ref(), &selection.selection))
        .collect::<Vec<_>>();
      paint_layout(
        layout.as_ref(),
        bounds,
        Some(&selection),
        drag_selection.as_ref(),
        show_caret,
        caret_width,
        local_caret_color_rgb,
        &external_carets,
        &external_selections,
        window,
        cx,
      );
    }
  }
}

fn caret_offset_belongs_to_chunk(layout: &LayoutState, offset: DocumentOffset) -> bool {
  layout.paragraphs.first().is_some_and(|paragraph| {
    if !paragraph.contains_byte(offset.byte) {
      return false;
    }
    offset.byte == paragraph.len
      || offset
        .byte
        .checked_add(1)
        .is_some_and(|next_byte| paragraph.contains_byte(next_byte))
  })
}

fn selection_belongs_to_chunk(layout: &LayoutState, selection: &EditorSelection) -> bool {
  let range = selection.normalized();
  layout
    .paragraphs
    .first()
    .is_some_and(|paragraph| range.start.paragraph <= paragraph.index && range.end.paragraph >= paragraph.index)
}

#[hotpath::measure_all]
impl Element for VirtualBlockElement {
  type RequestLayoutState = ();
  type PrepaintState = ();

  fn id(&self) -> Option<ElementId> {
    Some(structural_block_element_id(self.block_ix))
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
      layout_cell.set_layout(Rc::new(layout));
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
    self.layout.set_bounds(bounds);
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
    let Some((layout, bounds)) = self.layout.positioned() else {
      return;
    };
    for block in &layout.blocks {
      paint_structural_block(block, selected_block, table_cell_caret, text_selected, bounds.origin, window, cx);
    }
  }
}

#[hotpath::measure_all]
impl Element for EmptyVirtualItemElement {
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
    cx: &mut App,
  ) -> (LayoutId, Self::RequestLayoutState) {
    let mut style = Style::default();
    style.size.width = relative(1.0).into();
    style.size.height = relative(1.0).into();
    (window.request_layout(style, None, cx), ())
  }

  fn prepaint(
    &mut self,
    _id: Option<&GlobalElementId>,
    _inspector_id: Option<&InspectorElementId>,
    _bounds: Bounds<Pixels>,
    _request_layout: &mut Self::RequestLayoutState,
    _window: &mut Window,
    _cx: &mut App,
  ) {
  }

  fn paint(
    &mut self,
    _id: Option<&GlobalElementId>,
    _inspector_id: Option<&InspectorElementId>,
    _bounds: Bounds<Pixels>,
    _request_layout: &mut Self::RequestLayoutState,
    _prepaint: &mut Self::PrepaintState,
    _window: &mut Window,
    _cx: &mut App,
  ) {
  }
}

const STRUCTURAL_BLOCK_ELEMENT_ID_TAG: u64 = 1 << 63;

#[hotpath::measure]
fn paragraph_chunk_element_id(paragraph_ix: usize, chunk_ix: usize) -> ElementId {
  ElementId::Integer(packed_element_pair(paragraph_ix, chunk_ix) & !STRUCTURAL_BLOCK_ELEMENT_ID_TAG)
}

#[hotpath::measure]
fn structural_block_element_id(block_ix: usize) -> ElementId {
  ElementId::Integer(STRUCTURAL_BLOCK_ELEMENT_ID_TAG | (block_ix as u64 & !STRUCTURAL_BLOCK_ELEMENT_ID_TAG))
}

#[hotpath::measure]
fn packed_element_pair(first: usize, second: usize) -> u64 {
  ((first as u64 & 0x7fff_ffff) << 32) ^ (second as u64 & 0xffff_ffff)
}

#[hotpath::measure]
pub(super) fn request_word_layout(
  document: Document,
  layout_cell: WordElementLayout,
  invisibility_mode: bool,
  window: &mut Window,
) -> (LayoutId, ()) {
  let layout_id = window.request_measured_layout(Style::default(), move |known, available, window, cx| {
    let width = known
      .width
      .or(match available.width {
        AvailableSpace::Definite(width) => Some(width),
        _ => Some(px(900.0)),
      })
      .unwrap_or(px(900.0));
    let previous_layout = layout_cell.layout();
    let layout = build_layout_with_visibility(&document, width, previous_layout.as_deref(), invisibility_mode, window, cx);
    let size = layout.size;
    layout_cell.set_layout(Rc::new(layout));
    size
  });
  (layout_id, ())
}

// -------- Edit / movement helper free functions ------------------------
