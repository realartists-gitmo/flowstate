//! W-S4 P1: the pane tree. A window holds nested horizontal/vertical splits
//! whose leaves are panes; every pane is a tab strip + content area. The
//! tree REFERENCES panels by id and never owns entities — the `Workspace`
//! keeps today's flat ownership, so an in-window pane move is a pure tree
//! edit and a cross-window move stays the W-S3 handoff.
//!
//! Decided shape (all-A picks, 2026-07-16): freeform depth (physics, not
//! policy, caps it) · flows are pane-legal from P1 · empty panes host the
//! R5-B home · ctrl-\ / ctrl-shift-\ split.

use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, PartialOrd, Ord)]
pub(crate) struct PaneId(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SplitAxis {
  Horizontal,
  Vertical,
}

#[derive(Clone, Debug)]
pub(crate) enum PaneNode {
  Split {
    axis: SplitAxis,
    /// First child's share of the axis (P3 makes it draggable; P1 splits 50/50).
    ratio: f32,
    children: [Box<PaneNode>; 2],
  },
  Pane(PaneLeaf),
}

#[derive(Clone, Debug)]
pub(crate) struct PaneLeaf {
  pub id: PaneId,
  /// Panel ids (documents AND flows — Q2-A), in tab order.
  pub tab_order: Vec<Uuid>,
  pub active: Option<Uuid>,
}

/// The window's viewing surface: one tree + which leaf holds focus.
#[derive(Clone, Debug)]
pub(crate) struct PaneTree {
  root: PaneNode,
  pub focused: PaneId,
  next_pane: u64,
}

impl Default for PaneTree {
  fn default() -> Self {
    Self {
      root: PaneNode::Pane(PaneLeaf {
        id: PaneId(0),
        tab_order: Vec::new(),
        active: None,
      }),
      focused: PaneId(0),
      next_pane: 1,
    }
  }
}

impl PaneTree {
  fn mint(&mut self) -> PaneId {
    let id = PaneId(self.next_pane);
    self.next_pane += 1;
    id
  }

  pub fn leaf(&self, pane: PaneId) -> Option<&PaneLeaf> {
    fn walk(node: &PaneNode, pane: PaneId) -> Option<&PaneLeaf> {
      match node {
        PaneNode::Pane(leaf) => (leaf.id == pane).then_some(leaf),
        PaneNode::Split { children, .. } => walk(&children[0], pane).or_else(|| walk(&children[1], pane)),
      }
    }
    walk(&self.root, pane)
  }

  fn leaf_mut(&mut self, pane: PaneId) -> Option<&mut PaneLeaf> {
    fn walk(node: &mut PaneNode, pane: PaneId) -> Option<&mut PaneLeaf> {
      match node {
        PaneNode::Pane(leaf) => (leaf.id == pane).then_some(leaf),
        PaneNode::Split { children, .. } => {
          let [left, right] = children;
          walk(left, pane).or_else(|| walk(right, pane))
        },
      }
    }
    walk(&mut self.root, pane)
  }

  /// Leaves in layout order (left→right, top→bottom) — the focus-cycle order.
  pub fn leaves(&self) -> Vec<&PaneLeaf> {
    fn walk<'tree>(node: &'tree PaneNode, out: &mut Vec<&'tree PaneLeaf>) {
      match node {
        PaneNode::Pane(leaf) => out.push(leaf),
        PaneNode::Split { children, .. } => {
          walk(&children[0], out);
          walk(&children[1], out);
        },
      }
    }
    let mut out = Vec::new();
    walk(&self.root, &mut out);
    out
  }

  pub fn root(&self) -> &PaneNode {
    &self.root
  }

  pub fn pane_count(&self) -> usize {
    self.leaves().len()
  }

  /// One document, one pane (the W-S1 guard's law extended downward).
  pub fn pane_of(&self, panel_id: Uuid) -> Option<PaneId> {
    self
      .leaves()
      .into_iter()
      .find(|leaf| leaf.tab_order.contains(&panel_id))
      .map(|leaf| leaf.id)
  }

  pub fn focused_active(&self) -> Option<Uuid> {
    self.leaf(self.focused).and_then(|leaf| leaf.active)
  }

  /// A new tab lands in the FOCUSED pane and becomes its active tab.
  pub fn insert_tab(&mut self, panel_id: Uuid) {
    if self.pane_of(panel_id).is_some() {
      self.activate_tab(panel_id);
      return;
    }
    let focused = self.focused;
    if let Some(leaf) = self.leaf_mut(focused) {
      leaf.tab_order.push(panel_id);
      leaf.active = Some(panel_id);
    }
  }

  /// Activate a tab wherever it lives; its pane takes focus (the one-doc-
  /// one-pane law makes "activate" and "focus its pane" the same act).
  pub fn activate_tab(&mut self, panel_id: Uuid) -> bool {
    let Some(pane) = self.pane_of(panel_id) else {
      return false;
    };
    self.focused = pane;
    if let Some(leaf) = self.leaf_mut(pane) {
      leaf.active = Some(panel_id);
    }
    true
  }

  /// Remove a closed/handed-off tab. A pane whose LAST tab leaves collapses
  /// into its sibling (the sibling absorbs the space) — unless it is the
  /// root pane, which simply goes empty (the R5-B home stands there).
  pub fn remove_tab(&mut self, panel_id: Uuid) {
    let Some(pane) = self.pane_of(panel_id) else {
      return;
    };
    if let Some(leaf) = self.leaf_mut(pane) {
      leaf.tab_order.retain(|tab| *tab != panel_id);
      if leaf.active == Some(panel_id) {
        leaf.active = leaf.tab_order.last().copied();
      }
      if !leaf.tab_order.is_empty() {
        return;
      }
    }
    self.collapse_if_empty(pane);
  }

  /// Split `pane` on `axis`; the new sibling lands after (right/below). When
  /// the pane has an active tab it MOVES into the new pane ("split right
  /// with this tab"); an empty pane splits into two empties. The new pane
  /// takes focus. Returns the new pane's id.
  pub fn split(&mut self, pane: PaneId, axis: SplitAxis) -> Option<PaneId> {
    let new_id = self.mint();
    let moving = self.leaf_mut(pane).and_then(|leaf| {
      let moving = leaf.active.take();
      if let Some(tab) = moving {
        leaf.tab_order.retain(|candidate| *candidate != tab);
        leaf.active = leaf.tab_order.last().copied();
      }
      moving
    });

    fn split_node(node: &mut PaneNode, pane: PaneId, axis: SplitAxis, new_leaf: PaneLeaf) -> bool {
      match node {
        PaneNode::Pane(leaf) if leaf.id == pane => {
          let existing = PaneNode::Pane(leaf.clone());
          *node = PaneNode::Split {
            axis,
            ratio: 0.5,
            children: [Box::new(existing), Box::new(PaneNode::Pane(new_leaf))],
          };
          true
        },
        PaneNode::Pane(_) => false,
        PaneNode::Split { children, .. } => {
          let [left, right] = children;
          split_node(left, pane, axis, new_leaf.clone()) || split_node(right, pane, axis, new_leaf)
        },
      }
    }

    let new_leaf = PaneLeaf {
      id: new_id,
      tab_order: moving.into_iter().collect(),
      active: None,
    };
    let mut new_leaf_seeded = new_leaf;
    new_leaf_seeded.active = new_leaf_seeded.tab_order.last().copied();
    if !split_node(&mut self.root, pane, axis, new_leaf_seeded) {
      return None;
    }
    self.focused = new_id;
    Some(new_id)
  }

  /// Close a pane: surviving tabs move to the layout-order neighbor and the
  /// split collapses. Documents never close implicitly. The last pane
  /// refuses (there is always a viewing surface).
  pub fn close_pane(&mut self, pane: PaneId) -> bool {
    if self.pane_count() <= 1 {
      return false;
    }
    let Some(tabs) = self.leaf(pane).map(|leaf| leaf.tab_order.clone()) else {
      return false;
    };
    if !self.collapse_pane(pane) {
      return false;
    }
    let target = self.focused;
    if let Some(leaf) = self.leaf_mut(target) {
      for tab in tabs {
        if !leaf.tab_order.contains(&tab) {
          leaf.tab_order.push(tab);
        }
      }
      if leaf.active.is_none() {
        leaf.active = leaf.tab_order.last().copied();
      }
    }
    true
  }

  /// Focus the next pane in layout order (wraps).
  pub fn focus_next(&mut self) -> PaneId {
    let leaves = self.leaves();
    let current = leaves.iter().position(|leaf| leaf.id == self.focused).unwrap_or(0);
    let next = leaves[(current + 1) % leaves.len()].id;
    self.focused = next;
    next
  }

  fn collapse_if_empty(&mut self, pane: PaneId) {
    let is_empty = self
      .leaf(pane)
      .is_some_and(|leaf| leaf.tab_order.is_empty());
    if is_empty && self.pane_count() > 1 {
      self.collapse_pane(pane);
    }
  }

  /// Remove `pane` from the tree, promoting its sibling. Focus moves to the
  /// first leaf of the promoted sibling. False if `pane` is the root.
  fn collapse_pane(&mut self, pane: PaneId) -> bool {
    fn collapse(node: &mut PaneNode, pane: PaneId) -> bool {
      if let PaneNode::Split { children, .. } = node {
        for keep_ix in [1usize, 0] {
          let drop_ix = 1 - keep_ix;
          if matches!(children[drop_ix].as_ref(), PaneNode::Pane(leaf) if leaf.id == pane) {
            *node = std::mem::replace(children[keep_ix].as_mut(), PaneNode::Pane(PaneLeaf {
              id: PaneId(u64::MAX),
              tab_order: Vec::new(),
              active: None,
            }));
            return true;
          }
        }
        let [left, right] = children;
        return collapse(left, pane) || collapse(right, pane);
      }
      false
    }
    if !collapse(&mut self.root, pane) {
      return false;
    }
    if self.leaf(self.focused).is_none() {
      self.focused = self.leaves().first().map_or(PaneId(0), |leaf| leaf.id);
    }
    true
  }
}

impl PaneTree {
  /// P3: set a split's ratio by its DFS index (the render walk numbers
  /// splits in the same order).
  pub fn set_split_ratio(&mut self, split_ix: usize, ratio: f32) {
    fn walk(node: &mut PaneNode, counter: &mut usize, target: usize, ratio: f32) -> bool {
      if let PaneNode::Split { ratio: node_ratio, children, .. } = node {
        let this = *counter;
        *counter += 1;
        if this == target {
          *node_ratio = ratio.clamp(0.15, 0.85);
          return true;
        }
        let [left, right] = children;
        return walk(left, counter, target, ratio) || walk(right, counter, target, ratio);
      }
      false
    }
    let mut counter = 0;
    walk(&mut self.root, &mut counter, split_ix, ratio);
  }

  /// P3: move a tab into `target` (append; becomes its active; target takes
  /// focus). The source pane collapses if it empties.
  pub fn move_tab_to_pane(&mut self, panel_id: Uuid, target: PaneId) {
    if self.pane_of(panel_id) == Some(target) || self.leaf(target).is_none() {
      self.focused = target;
      return;
    }
    self.remove_tab(panel_id);
    self.focused = target;
    if let Some(leaf) = self.leaf_mut(target) {
      leaf.tab_order.push(panel_id);
      leaf.active = Some(panel_id);
    }
  }

  /// P3: the edge-drop gesture — split `target` on `axis` and land
  /// `panel_id` in the NEW pane, which takes focus.
  pub fn split_pane_with_tab(&mut self, target: PaneId, axis: SplitAxis, panel_id: Uuid) {
    if self.leaf(target).is_none() {
      return;
    }
    // A pane splitting around its own only tab would collapse mid-flight;
    // that gesture is just "no-op" (the tab already fills the pane).
    if self.pane_of(panel_id) == Some(target) && self.leaf(target).is_some_and(|leaf| leaf.tab_order.len() == 1) {
      self.focused = target;
      return;
    }
    self.remove_tab(panel_id);
    // The removal may have collapsed a DIFFERENT pane; target still exists
    // (it wasn't the one emptied unless it held only this tab — handled).
    let Some(new_pane) = self.split_target_empty(target, axis) else {
      // Split failed (target vanished) — fall back to the focused pane.
      let focused = self.focused;
      self.move_tab_to_pane(panel_id, focused);
      return;
    };
    if let Some(leaf) = self.leaf_mut(new_pane) {
      leaf.tab_order.push(panel_id);
      leaf.active = Some(panel_id);
    }
    self.focused = new_pane;
  }

  /// Split WITHOUT moving the target's active tab (the edge-drop brings its
  /// own cargo).
  fn split_target_empty(&mut self, pane: PaneId, axis: SplitAxis) -> Option<PaneId> {
    let new_id = self.mint();
    fn split_node(node: &mut PaneNode, pane: PaneId, axis: SplitAxis, new_leaf: PaneLeaf) -> bool {
      match node {
        PaneNode::Pane(leaf) if leaf.id == pane => {
          let existing = PaneNode::Pane(leaf.clone());
          *node = PaneNode::Split {
            axis,
            ratio: 0.5,
            children: [Box::new(existing), Box::new(PaneNode::Pane(new_leaf))],
          };
          true
        },
        PaneNode::Pane(_) => false,
        PaneNode::Split { children, .. } => {
          let [left, right] = children;
          split_node(left, pane, axis, new_leaf.clone()) || split_node(right, pane, axis, new_leaf)
        },
      }
    }
    let new_leaf = PaneLeaf {
      id: new_id,
      tab_order: Vec::new(),
      active: None,
    };
    split_node(&mut self.root, pane, axis, new_leaf).then_some(new_id)
  }
}

/// W-S4 P4: the persisted pane layout — the tree with panel ids swapped for
/// session ENTRY INDICES (the `pinned_entry_indices` trick). Pathless tabs
/// drop at persist; entries that fail to restore drop at load.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) enum PaneLayoutEntry {
  Split {
    axis: PaneLayoutAxis,
    ratio: f32,
    children: Vec<PaneLayoutEntry>,
  },
  Pane { tabs: Vec<usize>, active: Option<usize> },
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) enum PaneLayoutAxis {
  Horizontal,
  Vertical,
}

impl PaneTree {
  /// `None` for a single pane, so plain sessions keep their old shape.
  pub fn to_layout(&self, entry_index_of: &std::collections::HashMap<Uuid, usize>) -> Option<PaneLayoutEntry> {
    if self.pane_count() <= 1 {
      return None;
    }
    fn walk(node: &PaneNode, entry_index_of: &std::collections::HashMap<Uuid, usize>) -> PaneLayoutEntry {
      match node {
        PaneNode::Pane(leaf) => PaneLayoutEntry::Pane {
          tabs: leaf
            .tab_order
            .iter()
            .filter_map(|id| entry_index_of.get(id).copied())
            .collect(),
          active: leaf.active.and_then(|id| entry_index_of.get(&id).copied()),
        },
        PaneNode::Split { axis, ratio, children } => PaneLayoutEntry::Split {
          axis: match axis {
            SplitAxis::Horizontal => PaneLayoutAxis::Horizontal,
            SplitAxis::Vertical => PaneLayoutAxis::Vertical,
          },
          ratio: *ratio,
          children: vec![walk(&children[0], entry_index_of), walk(&children[1], entry_index_of)],
        },
      }
    }
    Some(walk(&self.root, entry_index_of))
  }

  /// Rebuild from a persisted layout plus the per-entry restored panel ids
  /// (`restored[i]` = the panel entry `i` became, `None` = failed to open).
  /// Empty panes drop, single-child splits collapse, and a tree with
  /// nothing left returns `None` (the caller keeps the default single pane).
  pub fn from_layout(layout: &PaneLayoutEntry, restored: &[Option<Uuid>]) -> Option<Self> {
    let mut next_pane = 0u64;
    fn build(layout: &PaneLayoutEntry, restored: &[Option<Uuid>], next_pane: &mut u64) -> Option<PaneNode> {
      match layout {
        PaneLayoutEntry::Pane { tabs, active } => {
          let tab_order: Vec<Uuid> = tabs
            .iter()
            .filter_map(|entry_ix| restored.get(*entry_ix).copied().flatten())
            .collect();
          if tab_order.is_empty() {
            return None;
          }
          let active = active
            .and_then(|entry_ix| restored.get(entry_ix).copied().flatten())
            .filter(|id| tab_order.contains(id))
            .or_else(|| tab_order.last().copied());
          let id = PaneId(*next_pane);
          *next_pane += 1;
          Some(PaneNode::Pane(PaneLeaf { id, tab_order, active }))
        },
        PaneLayoutEntry::Split { axis, ratio, children } => {
          let mut built: Vec<PaneNode> = children
            .iter()
            .filter_map(|child| build(child, restored, next_pane))
            .collect();
          match built.len() {
            0 => None,
            1 => Some(built.remove(0)),
            _ => {
              let second = built.remove(1);
              let first = built.remove(0);
              Some(PaneNode::Split {
                axis: match axis {
                  PaneLayoutAxis::Horizontal => SplitAxis::Horizontal,
                  PaneLayoutAxis::Vertical => SplitAxis::Vertical,
                },
                ratio: ratio.clamp(0.15, 0.85),
                children: [Box::new(first), Box::new(second)],
              })
            },
          }
        },
      }
    }
    let root = build(layout, restored, &mut next_pane)?;
    let mut tree = Self {
      root,
      focused: PaneId(0),
      next_pane,
    };
    tree.focused = tree.leaves().first().map(|leaf| leaf.id)?;
    Some(tree)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn id(n: u128) -> Uuid {
    Uuid::from_u128(n)
  }

  #[test]
  fn tabs_land_in_the_focused_pane_and_one_doc_one_pane_holds() {
    let mut tree = PaneTree::default();
    tree.insert_tab(id(1));
    tree.insert_tab(id(2));
    assert_eq!(tree.pane_count(), 1);
    assert_eq!(tree.focused_active(), Some(id(2)));
    // Re-inserting an open doc ACTIVATES it instead of duplicating.
    tree.insert_tab(id(1));
    assert_eq!(tree.leaf(tree.focused).unwrap().tab_order.len(), 2);
    assert_eq!(tree.focused_active(), Some(id(1)));
  }

  #[test]
  fn split_moves_the_active_tab_and_takes_focus() {
    let mut tree = PaneTree::default();
    tree.insert_tab(id(1));
    tree.insert_tab(id(2));
    let root_pane = tree.focused;
    let new_pane = tree.split(root_pane, SplitAxis::Horizontal).expect("split");
    assert_eq!(tree.pane_count(), 2);
    assert_eq!(tree.focused, new_pane, "the new pane takes focus");
    assert_eq!(tree.focused_active(), Some(id(2)), "the active tab moved with the split");
    assert_eq!(
      tree.leaf(root_pane).unwrap().active,
      Some(id(1)),
      "the source pane falls back to its remaining tab"
    );
    assert_eq!(tree.pane_of(id(2)), Some(new_pane), "one doc, one pane");
  }

  #[test]
  fn closing_the_last_tab_collapses_the_split() {
    let mut tree = PaneTree::default();
    tree.insert_tab(id(1));
    tree.insert_tab(id(2));
    let new_pane = tree.split(tree.focused, SplitAxis::Vertical).expect("split");
    tree.remove_tab(id(2));
    assert_eq!(tree.pane_count(), 1, "the emptied pane collapsed into its sibling");
    assert!(tree.leaf(new_pane).is_none());
    assert_eq!(tree.focused_active(), Some(id(1)));
    // The ROOT pane never collapses — it goes empty (the home stands there).
    tree.remove_tab(id(1));
    assert_eq!(tree.pane_count(), 1);
    assert_eq!(tree.focused_active(), None);
  }

  #[test]
  fn close_pane_moves_survivors_and_the_last_pane_refuses() {
    let mut tree = PaneTree::default();
    tree.insert_tab(id(1));
    tree.insert_tab(id(2));
    let new_pane = tree.split(tree.focused, SplitAxis::Horizontal).expect("split");
    tree.insert_tab(id(3)); // lands in the focused (new) pane
    assert!(tree.close_pane(new_pane), "closing a non-root pane succeeds");
    assert_eq!(tree.pane_count(), 1);
    let survivor = tree.leaf(tree.focused).unwrap();
    assert!(survivor.tab_order.contains(&id(2)) && survivor.tab_order.contains(&id(3)));
    assert!(!tree.close_pane(tree.focused), "the last pane refuses to close");
  }

  #[test]
  fn tab_drops_move_or_split_and_ratios_address_splits_in_dfs_order() {
    let mut tree = PaneTree::default();
    tree.insert_tab(id(1));
    tree.insert_tab(id(2));
    let left = tree.focused;
    let right = tree.split(left, SplitAxis::Horizontal).expect("split");

    // Center drop: the tab moves panes, the emptied source collapses, and
    // the target keeps focus.
    tree.move_tab_to_pane(id(1), right);
    assert_eq!(tree.pane_of(id(1)), Some(right));
    assert_eq!(tree.focused, right);
    assert_eq!(tree.pane_count(), 1, "the emptied source collapsed away");

    // Edge drop: split the pane around the dragged tab; the NEW pane gets it.
    let target = tree.pane_of(id(2)).expect("pane");
    tree.split_pane_with_tab(target, SplitAxis::Vertical, id(1));
    assert_eq!(tree.pane_count(), 2);
    let new_pane = tree.pane_of(id(1)).expect("new pane");
    assert_ne!(new_pane, target);
    assert_eq!(tree.focused, new_pane);

    // Ratio addressing follows DFS order (one split here → index 0).
    tree.set_split_ratio(0, 0.7);
    let PaneNode::Split { ratio, .. } = tree.root() else {
      panic!("expected a split root");
    };
    assert!((ratio - 0.7).abs() < f32::EPSILON);
  }

  #[test]
  fn layout_round_trips_through_entry_indices() {
    let mut tree = PaneTree::default();
    tree.insert_tab(id(1));
    tree.insert_tab(id(2));
    tree.split(tree.focused, SplitAxis::Horizontal);
    tree.insert_tab(id(3));

    let mut index_of = std::collections::HashMap::new();
    index_of.insert(id(1), 0usize);
    index_of.insert(id(2), 1usize);
    index_of.insert(id(3), 2usize);
    let layout = tree.to_layout(&index_of).expect("multi-pane layouts persist");

    // Full restore: same shape, same actives.
    let restored = vec![Some(id(1)), Some(id(2)), Some(id(3))];
    let rebuilt = PaneTree::from_layout(&layout, &restored).expect("rebuilds");
    assert_eq!(rebuilt.pane_count(), 2);
    assert_eq!(rebuilt.pane_of(id(1)), rebuilt.leaves().first().map(|leaf| leaf.id));
    assert_eq!(rebuilt.pane_of(id(2)), rebuilt.pane_of(id(3)), "the split pane kept both tabs");

    // Entry 2 fails to open → its tab drops but the pane survives on id(2).
    let partial = vec![Some(id(1)), Some(id(2)), None];
    let rebuilt = PaneTree::from_layout(&layout, &partial).expect("tolerant rebuild");
    assert_eq!(rebuilt.pane_count(), 2);
    assert_eq!(rebuilt.pane_of(id(3)), None);

    // The whole second pane fails → the split collapses to a single pane.
    let collapsed = vec![Some(id(1)), None, None];
    let rebuilt = PaneTree::from_layout(&layout, &collapsed).expect("collapses to one pane");
    assert_eq!(rebuilt.pane_count(), 1);

    // Nothing restored → None; the caller keeps the default tree.
    assert!(PaneTree::from_layout(&layout, &[None, None, None]).is_none());
  }

  #[test]
  fn focus_cycles_in_layout_order() {
    let mut tree = PaneTree::default();
    tree.insert_tab(id(1));
    let first = tree.focused;
    tree.split(first, SplitAxis::Horizontal);
    let second = tree.focused;
    tree.split(second, SplitAxis::Vertical);
    let third = tree.focused;
    assert_eq!(tree.focus_next(), first);
    assert_eq!(tree.focus_next(), second);
    assert_eq!(tree.focus_next(), third);
  }
}
