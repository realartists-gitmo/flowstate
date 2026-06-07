#[derive(Clone)]
struct OutlineCache {
  document_id: Uuid,
  edit_generation: u64,
  signature: OutlineSignature,
  signature_scratch: Vec<OutlineEntry>,
  visible_revision: u64,
  nodes: Rc<Vec<OutlineNode>>,
  visible_paragraphs: Vec<usize>,
  row_guides: Rc<Vec<OutlineRowGuides>>,
  tree_items: Vec<TreeItem>,
}

#[hotpath::measure_all]
impl OutlineCache {
  fn new(document_id: Uuid, edit_generation: u64, signature: OutlineSignature) -> Self {
    let nodes = outline_nodes_from_entries(&signature.entries);
    Self {
      document_id,
      edit_generation,
      signature,
      signature_scratch: Vec::new(),
      visible_revision: u64::MAX,
      nodes: Rc::new(nodes),
      visible_paragraphs: Vec::new(),
      row_guides: Rc::new(Vec::new()),
      tree_items: Vec::new(),
    }
  }

  fn rebuild_visible(&mut self, revision: u64, collapsed_items: &HashSet<usize>) {
    self.visible_paragraphs.clear();
    collect_visible_outline_paragraphs(&self.nodes, collapsed_items, &mut self.visible_paragraphs);
    let mut row_guides = Vec::with_capacity(self.visible_paragraphs.len());
    collect_visible_outline_guides(&self.nodes, collapsed_items, &mut row_guides);
    self.row_guides = Rc::new(row_guides);
    self.tree_items = self
      .nodes
      .iter()
      .map(|node| outline_node_to_tree_item(node, collapsed_items))
      .collect();
    self.visible_revision = revision;
  }

  fn update_signature(&mut self, document: &Document, edit_generation: u64) -> bool {
    let paragraph_count = outline_signature_entries_into(document, &mut self.signature_scratch);
    let unchanged = self.signature.paragraph_count == paragraph_count && self.signature.entries == self.signature_scratch;
    self.signature.paragraph_count = paragraph_count;
    std::mem::swap(&mut self.signature.entries, &mut self.signature_scratch);
    self.edit_generation = edit_generation;
    if unchanged {
      return false;
    }

    self.nodes = Rc::new(outline_nodes_from_entries(&self.signature.entries));
    self.visible_revision = u64::MAX;
    true
  }
}

#[derive(Clone, PartialEq, Eq)]
struct OutlineSignature {
  paragraph_count: usize,
  entries: Vec<OutlineEntry>,
}

#[derive(Clone, PartialEq, Eq)]
struct OutlineEntry {
  paragraph_ix: usize,
  level: usize,
  text: String,
}

#[derive(Clone)]
struct OutlineNode {
  paragraph_ix: usize,
  level: usize,
  text: String,
  children: Vec<OutlineNode>,
}

#[derive(Clone, Default)]
struct OutlineRowGuides {
  ancestor_depths: Vec<usize>,
  extends_from_toggle: bool,
}

#[hotpath::measure]
fn insert_outline_node(nodes: &mut Vec<OutlineNode>, level: usize, node: OutlineNode) {
  if level == 0 {
    nodes.push(node);
    return;
  }

  if let Some(parent) = nodes
    .iter_mut()
    .rev()
    .find(|candidate| candidate.level < level)
  {
    insert_outline_node(&mut parent.children, level, node);
  } else {
    nodes.push(node);
  }
}

#[hotpath::measure]
fn outline_node_to_tree_item(node: &OutlineNode, collapsed_items: &HashSet<usize>) -> TreeItem {
  let paragraph_ix = node.paragraph_ix;
  TreeItem::new(outline_item_id(paragraph_ix), node.text.clone())
    .children(
      node
        .children
        .iter()
        .map(|child| outline_node_to_tree_item(child, collapsed_items)),
    )
    .expanded(!collapsed_items.contains(&paragraph_ix))
    .disabled(true)
}

#[cfg(test)]
#[hotpath::measure]
fn outline_nodes(document: &Document) -> Vec<OutlineNode> {
  let signature = outline_signature(document);
  outline_nodes_from_entries(&signature.entries)
}

#[hotpath::measure]
fn outline_nodes_from_entries(entries: &[OutlineEntry]) -> Vec<OutlineNode> {
  let mut roots = Vec::<OutlineNode>::new();
  for entry in entries {
    insert_outline_node(
      &mut roots,
      entry.level,
      OutlineNode {
        paragraph_ix: entry.paragraph_ix,
        level: entry.level,
        text: entry.text.clone(),
        children: Vec::new(),
      },
    );
  }
  roots
}

#[hotpath::measure]
fn outline_signature(document: &Document) -> OutlineSignature {
  let mut entries = Vec::new();
  let paragraph_count = outline_signature_entries_into(document, &mut entries);
  OutlineSignature { paragraph_count, entries }
}

#[hotpath::measure]
fn outline_signature_entries_into(document: &Document, entries: &mut Vec<OutlineEntry>) -> usize {
  let mut entry_ix = 0usize;
  for (paragraph_ix, paragraph) in document.paragraphs.iter().enumerate() {
    let Some(level) = outline_level(paragraph.style) else {
      continue;
    };
    if let Some(entry) = entries.get_mut(entry_ix) {
      entry.paragraph_ix = paragraph_ix;
      entry.level = level;
      outline_paragraph_label_into(document, paragraph_ix, &mut entry.text);
    } else {
      let mut text = String::with_capacity(80);
      outline_paragraph_label_into(document, paragraph_ix, &mut text);
      entries.push(OutlineEntry { paragraph_ix, level, text });
    }
    entry_ix += 1;
  }
  entries.truncate(entry_ix);

  document.paragraphs.len()
}

#[hotpath::measure]
fn collect_visible_outline_paragraphs(nodes: &[OutlineNode], collapsed_items: &HashSet<usize>, output: &mut Vec<usize>) {
  for node in nodes {
    output.push(node.paragraph_ix);
    if !collapsed_items.contains(&node.paragraph_ix) {
      collect_visible_outline_paragraphs(&node.children, collapsed_items, output);
    }
  }
}

fn collect_visible_outline_guides(nodes: &[OutlineNode], collapsed_items: &HashSet<usize>, output: &mut Vec<OutlineRowGuides>) {
  collect_visible_outline_guides_with_ancestors(nodes, collapsed_items, 0, &mut Vec::new(), output);
}

fn collect_visible_outline_guides_with_ancestors(
  nodes: &[OutlineNode],
  collapsed_items: &HashSet<usize>,
  depth: usize,
  ancestor_depths: &mut Vec<usize>,
  output: &mut Vec<OutlineRowGuides>,
) {
  for node in nodes {
    let is_expanded_folder = !node.children.is_empty() && !collapsed_items.contains(&node.paragraph_ix);
    output.push(OutlineRowGuides {
      ancestor_depths: ancestor_depths.clone(),
      extends_from_toggle: is_expanded_folder,
    });

    if is_expanded_folder {
      ancestor_depths.push(depth);
      collect_visible_outline_guides_with_ancestors(&node.children, collapsed_items, depth + 1, ancestor_depths, output);
      ancestor_depths.pop();
    }
  }
}

#[hotpath::measure]
fn active_visible_outline_paragraph_from_visible(visible_paragraphs: &[usize], caret_paragraph: usize) -> Option<usize> {
  let mut low = 0usize;
  let mut high = visible_paragraphs.len();
  while low < high {
    let mid = low + (high - low) / 2;
    if visible_paragraphs[mid] <= caret_paragraph {
      low = mid + 1;
    } else {
      high = mid;
    }
  }
  low
    .checked_sub(1)
    .and_then(|ix| visible_paragraphs.get(ix).copied())
}

struct OutlineContextMenu {
  position: Point<Pixels>,
  menu_view: Entity<PopupMenu>,
  _subscription: Subscription,
}

fn outline_level_name(level: usize) -> &'static str {
  match level {
    0 => "Pocket",
    1 => "Hat",
    2 => "Block",
    3 => "Tag / Analytic",
    _ => "Entry",
  }
}

#[hotpath::measure]
fn outline_level(style: ParagraphStyle) -> Option<usize> {
  match style {
    flowstate_document::PARAGRAPH_POCKET => Some(0),
    flowstate_document::PARAGRAPH_HAT => Some(1),
    flowstate_document::PARAGRAPH_BLOCK => Some(2),
    flowstate_document::PARAGRAPH_TAG | flowstate_document::PARAGRAPH_ANALYTIC => Some(3),
    ParagraphStyle::Normal | flowstate_document::PARAGRAPH_UNDERTAG | ParagraphStyle::Custom(_) => None,
  }
}

#[hotpath::measure]
fn outline_item_id(paragraph_ix: usize) -> String {
  format!("paragraph:{paragraph_ix}")
}

#[hotpath::measure]
fn outline_paragraph_ix(id: &str) -> Option<usize> {
  id.strip_prefix("paragraph:")?.parse().ok()
}

#[cfg(test)]
#[hotpath::measure]
fn outline_paragraph_label(document: &Document, paragraph_ix: usize) -> String {
  let mut label = String::with_capacity(80);
  outline_paragraph_label_into(document, paragraph_ix, &mut label);
  label
}

#[hotpath::measure]
fn outline_paragraph_label_into(document: &Document, paragraph_ix: usize, label: &mut String) {
  let paragraph_range = paragraph_byte_range(document, paragraph_ix);
  const MAX_BYTES: usize = 80;
  const TRUNCATED_BYTES: usize = MAX_BYTES - 3;
  label.clear();
  let mut pending_space = false;
  let mut truncated = false;

  'chunks: for chunk in document.text.byte_slice(paragraph_range).chunks() {
    for ch in chunk.chars() {
      if ch.is_whitespace() {
        pending_space = !label.is_empty();
        continue;
      }

      let space_len = usize::from(pending_space && !label.is_empty());
      if label.len() + space_len + ch.len_utf8() > TRUNCATED_BYTES {
        truncated = true;
        break 'chunks;
      }
      if pending_space && !label.is_empty() {
        label.push(' ');
      }
      label.push(ch);
      pending_space = false;
    }
  }

  if label.is_empty() {
    label.push_str("(empty)");
  } else if truncated {
    label.push_str("...");
  }
}

#[hotpath::measure]
fn outline_label_width(nav_width: Pixels, depth: usize) -> Pixels {
  // Mirrors the outline row layout: nav padding, row indentation, disclosure
  // slot, row gap, and right padding are fixed, so the remaining width is the
  // label rect. Keeping this deterministic avoids a first-paint measure/notify
  // cycle that visibly moves the tree after startup.
  (nav_width - px(56.0) - px(12.0) * depth).max(px(32.0))
}

#[hotpath::measure]
fn outline_label_text_width(label_width: Pixels, window: &Window) -> Pixels {
  // The measured blue label rect includes `.px_1()` padding on both sides.
  // Truncation must target the inner text box, with a small paint tolerance so
  // the suffix glyph does not get clipped by the label's overflow boundary.
  (label_width - window.rem_size() * 0.5 - px(2.0)).max(px(1.0))
}

#[hotpath::measure]
fn truncate_outline_label(label: &str, width: Pixels, window: &mut Window, cx: &mut App) -> SharedString {
  let text_style = window.text_style();
  // Keep this in sync with the outline row's `.text_xs()` style. GPUI's text
  // helper defines text_xs as 0.75rem; using the default 1rem style here makes
  // the app-level truncator think the label is much wider than it renders.
  let font_size = window.rem_size() * 0.75;
  let mut runs = vec![text_style.to_run(label.len())];
  cx.text_system()
    .line_wrapper(text_style.font(), font_size)
    .truncate_line(label.to_string().into(), width, "…", &mut runs)
}
