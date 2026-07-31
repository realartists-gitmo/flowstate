use std::{cell::RefCell, rc::Rc};

use gpui::{
  App, AvailableSpace, Background, Bounds, Element, ElementId, ElementInputHandler, Entity, GlobalElementId, InspectorElementId, IntoElement,
  LayoutId, Pixels, Style, Window, fill, px, relative,
};

use super::*;

pub struct RichTextDocumentElement {
  pub(super) document: DocumentProjection,
  pub(super) layout: WordElementLayout,
  pub(super) invisibility_mode: bool,
}

#[hotpath::measure_all]
impl RichTextDocumentElement {
  pub fn new(document: DocumentProjection) -> Self {
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
  /// Accessibility description of this paragraph, resolved during
  /// `request_layout` because `Element::a11y_role` takes only `&self` and so
  /// cannot read the editor entity itself. `None` when accessibility is
  /// inactive (the common case — nothing here is observable without assistive
  /// technology) or when this is a continuation chunk.
  pub(super) a11y: Option<A11yParagraphInfo>,
}

#[derive(Clone)]
pub(super) struct VirtualBlockElement {
  pub(super) editor: Entity<RichTextEditor>,
  pub(super) block_ix: usize,
  pub(super) layout: WordElementLayout,
  /// Accessibility description of this block; see the note on
  /// `VirtualParagraphChunkElement::a11y` for why it is resolved eagerly.
  pub(super) a11y: Option<A11yBlockInfo>,
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
      paint_layout(
        layout.as_ref(),
        bounds,
        None,
        None,
        false,
        px(1.0),
        None,
        gpui::black(),
        &[],
        &[],
        &[],
        &[],
        None,
        window,
        cx,
      );
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

  /// `Paragraph`, or `Heading` when the paragraph's style resolves to a section
  /// heading. `None` — meaning no node at all — for continuation chunks and
  /// whenever accessibility is inactive.
  fn a11y_role(&self) -> Option<gpui::Role> {
    self.a11y.as_ref().map(|info| info.role)
  }

  fn write_a11y_info(&self, node: &mut gpui::accesskit::Node) {
    if let Some(level) = self.a11y.as_ref().and_then(|info| info.level) {
      node.set_level(level);
    }
  }

  /// Expose the paragraph's text as `TextRun` children, plus the caret /
  /// selection when it lands in this paragraph.
  ///
  /// These are flat leaves under this paragraph's node — `push_child` cannot
  /// nest — which is exactly the shape AccessKit's text pattern wants: a
  /// container whose children are runs, with `text_selection` on the container
  /// pointing into them.
  fn a11y_synthetic_children(&mut self, _prepaint: &mut Self::PrepaintState, builder: &mut gpui::A11ySubtreeBuilder) {
    let Some(info) = self.a11y.as_ref() else {
      return;
    };

    // Spans break at run-style boundaries as well as the 255-char limit, so a
    // citation or highlight is its own node carrying its own presentation.
    let chunks: Vec<&str> = info.spans.iter().map(|span| span.text.as_str()).collect();
    let ids: Vec<gpui::accesskit::NodeId> = (0..chunks.len()).map(|ix| builder.synthetic_node_id(ix)).collect();

    for (ix, chunk) in chunks.iter().enumerate() {
      let span = &info.spans[ix];
      let mut node = a11y_text_run_node(chunk);
      if span.underline {
        // Colour the decoration with the span's own foreground when it has one,
        // so an underline inherits the style it belongs to.
        node.set_underline(gpui::accesskit::TextDecoration {
          style: gpui::accesskit::TextDecorationStyle::Solid,
          color: span.foreground.unwrap_or(gpui::accesskit::Color {
            red: 0,
            green: 0,
            blue: 0,
            alpha: 255,
          }),
        });
      }
      if let Some(description) = span.role_description {
        node.set_role_description(description);
      }
      if let Some(color) = span.foreground {
        node.set_foreground_color(color);
      }
      if let Some(color) = span.background {
        node.set_background_color(color);
      }
      // Link the runs so assistive technology treats a chunked paragraph as one
      // continuous line rather than several unrelated fragments.
      if ix > 0 {
        node.set_previous_on_line(ids[ix - 1]);
      }
      if ix + 1 < ids.len() {
        node.set_next_on_line(ids[ix + 1]);
      }
      builder.push_child(ids[ix], node);
    }

    if let Some((start_char, end_char)) = info.selection {
      // Map an absolute character index onto (run, index-within-run). A position
      // exactly on a boundary is reported at the END of the earlier run, which
      // is what AccessKit expects for a caret sitting between two runs.
      let locate = |char_ix: usize| -> gpui::accesskit::TextPosition {
        let mut remaining = char_ix;
        for (ix, chunk) in chunks.iter().enumerate() {
          let len = chunk.chars().count();
          if remaining <= len {
            return gpui::accesskit::TextPosition {
              node: ids[ix],
              character_index: remaining,
            };
          }
          remaining -= len;
        }
        gpui::accesskit::TextPosition {
          node: *ids.last().expect("a11y_text_runs always yields at least one run"),
          character_index: chunks.last().map_or(0, |c| c.chars().count()),
        }
      };
      builder.parent_node().set_text_selection(gpui::accesskit::TextSelection {
        anchor: locate(start_char),
        focus: locate(end_char),
      });
    }
  }

  fn request_layout(
    &mut self,
    _id: Option<&GlobalElementId>,
    _inspector_id: Option<&InspectorElementId>,
    window: &mut Window,
    _cx: &mut App,
  ) -> (LayoutId, Self::RequestLayoutState) {
    // Resolve accessibility here: `a11y_role()` runs before `prepaint` and gets
    // only `&self`, so it cannot read the editor entity. Skipped entirely when
    // no assistive technology is attached.
    self.a11y = if window.is_a11y_active() {
      self.editor.read(_cx).a11y_paragraph_info(self.paragraph_ix, self.chunk_ix)
    } else {
      None
    };

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
    let (
      selection,
      drag_selection,
      caret_offset,
      caret_width,
      caret_color_rgb,
      default_caret_color,
      external_carets,
      external_selections,
      annotation_selections,
      search_highlights,
      active_search_highlight,
    ) = {
      let editor = self.editor.read(cx);
      let drag_selection = editor.drag_source_selection();
      let external_carets = editor.external_carets_for_paragraph(self.paragraph_ix);
      let external_selections = editor.external_selections_for_paragraph(self.paragraph_ix);
      let annotation_selections = editor.annotation_selections_for_paragraph(self.paragraph_ix);
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
        editor.local_caret_color_rgb(),
        // The caret's fallback color = the theme's default text color, so it
        // contrasts with the background (a hardcoded black caret is invisible on a
        // dark theme).
        editor.document.theme.default_text_color,
        external_carets,
        external_selections,
        annotation_selections,
        editor.search_highlights.clone(),
        editor.active_search_highlight,
      )
    };
    if let Some((layout, bounds)) = self.layout.positioned() {
      let focus_handle = self.editor.read(cx).focus_handle.clone();
      window.handle_input(&focus_handle, ElementInputHandler::new(bounds, self.editor.clone()), cx);
      if self.chunk_ix == 0 {
        let collapse_state = self
          .editor
          .read(cx)
          .section_collapse_state_at_paragraph(self.paragraph_ix, &[0, 1, 2, 3]);
        if let Some(collapsed) = collapse_state {
          let indicator_width = px(9.0);
          let indicator_height = if collapsed { px(2.0) } else { px(9.0) };
          let (indicator_x, indicator_y) = layout
            .paragraphs
            .first()
            .and_then(|paragraph| {
              paragraph.lines.last().map(|line| {
                (
                  line.origin.x + line.width + px(6.0),
                  line.origin.y + ((line.line_height - indicator_height) / 2.0).max(px(0.0)),
                )
              })
            })
            .unwrap_or((px(6.0), px(6.0)));
          let indicator = Bounds::new(
            gpui::point(bounds.left() + indicator_x, bounds.top() + indicator_y),
            gpui::size(indicator_width, indicator_height),
          );
          window.paint_quad(fill(indicator, Background::from(gpui::black().opacity(0.7))));
        }
      }
      let show_caret = caret_offset.is_some_and(|offset| caret_offset_belongs_to_chunk(layout.as_ref(), offset));
      let external_carets = external_carets
        .into_iter()
        .filter(|caret| caret_offset_belongs_to_chunk(layout.as_ref(), caret.offset))
        .collect::<Vec<_>>();
      paint_layout(
        layout.as_ref(),
        bounds,
        Some(&selection),
        drag_selection.as_ref(),
        show_caret,
        caret_width,
        caret_color_rgb,
        default_caret_color,
        &external_carets,
        &external_selections,
        &annotation_selections,
        &search_highlights,
        active_search_highlight,
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

  /// `Image`, `Math` or `Table` — the non-text content of the document, which
  /// would otherwise be entirely invisible to assistive technology.
  fn a11y_role(&self) -> Option<gpui::Role> {
    self.a11y.as_ref().map(|info| info.role)
  }

  fn write_a11y_info(&self, node: &mut gpui::accesskit::Node) {
    let Some(info) = self.a11y.as_ref() else {
      return;
    };
    if !info.label.is_empty() {
      node.set_label(info.label.clone());
    }
    if let Some((rows, columns)) = info.table_shape {
      node.set_row_count(rows);
      node.set_column_count(columns);
    }
  }

  fn request_layout(
    &mut self,
    _id: Option<&GlobalElementId>,
    _inspector_id: Option<&InspectorElementId>,
    window: &mut Window,
    _cx: &mut App,
  ) -> (LayoutId, Self::RequestLayoutState) {
    self.a11y = if window.is_a11y_active() {
      self.editor.read(_cx).a11y_block_info(self.block_ix)
    } else {
      None
    };

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
  document: DocumentProjection,
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
