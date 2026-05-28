#[derive(Clone)]
struct OutlineCache {
  document_id: Uuid,
  signature: OutlineSignature,
  visible_revision: u64,
  nodes: Rc<Vec<OutlineNode>>,
  visible_paragraphs: Vec<usize>,
  tree_items: Vec<TreeItem>,
}

#[hotpath::measure_all]
impl OutlineCache {
  fn new(document_id: Uuid, signature: OutlineSignature) -> Self {
    let nodes = outline_nodes_from_entries(&signature.entries);
    Self {
      document_id,
      signature,
      visible_revision: u64::MAX,
      nodes: Rc::new(nodes),
      visible_paragraphs: Vec::new(),
      tree_items: Vec::new(),
    }
  }

  fn rebuild_visible(&mut self, revision: u64, collapsed_items: &HashSet<usize>) {
    self.visible_paragraphs.clear();
    collect_visible_outline_paragraphs(&self.nodes, collapsed_items, &mut self.visible_paragraphs);
    self.tree_items = self
      .nodes
      .iter()
      .map(|node| outline_node_to_tree_item(node, collapsed_items))
      .collect();
    self.visible_revision = revision;
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
  for (paragraph_ix, paragraph) in document.paragraphs.iter().enumerate() {
    let Some(level) = outline_level(paragraph.style) else {
      continue;
    };
    entries.push(OutlineEntry {
      paragraph_ix,
      level,
      text: outline_paragraph_label(document, paragraph_ix),
    });
  }

  OutlineSignature {
    paragraph_count: document.paragraphs.len(),
    entries,
  }
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

#[hotpath::measure]
fn outline_level(style: ParagraphStyle) -> Option<usize> {
  match style {
    ParagraphStyle::Pocket => Some(0),
    ParagraphStyle::Hat => Some(1),
    ParagraphStyle::Block => Some(2),
    ParagraphStyle::Tag | ParagraphStyle::Analytic => Some(3),
    ParagraphStyle::Normal | ParagraphStyle::Undertag => None,
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

#[hotpath::measure]
fn outline_paragraph_label(document: &Document, paragraph_ix: usize) -> String {
  let paragraph_range = paragraph_byte_range(document, paragraph_ix);
  const MAX_BYTES: usize = 80;
  const TRUNCATED_BYTES: usize = MAX_BYTES - 3;
  let mut label = String::new();
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
    "(empty)".to_string()
  } else if truncated {
    label.push_str("...");
    label
  } else {
    label
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

