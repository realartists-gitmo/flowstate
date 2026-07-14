//! This map a Range<usize> to a set of style
//!

use std::{
    ops::{ControlFlow, Deref, DerefMut, Range, RangeBounds},
    sync::Arc,
};

use generic_btree::{
    rle::{CanRemove, HasLength, Mergeable, Sliceable, TryInsert},
    BTree, BTreeTrait, ElemSlice, LengthFinder, UseLengthFinder,
};
use rustc_hash::FxHashMap;

use once_cell::sync::Lazy;

use crate::delta::StyleMeta;

use super::{AnchorType, StyleKey, StyleOp};

/// This struct keep the mapping of ranges to numbers
///
/// It's initialized with usize::MAX/2 length.
#[derive(Debug, Clone)]
pub(super) struct StyleRangeMap {
    pub(super) tree: BTree<RangeNumMapTrait>,
    has_style: bool,
    /// §flowstate stylemap patch: newest `(lamport, peer)` op ever inserted
    /// into this map. An annotate whose op is newer than EVERYTHING ever
    /// inserted cannot be a duplicate anywhere — the whole covered-elem walk
    /// skips per-elem membership checks on one O(1) compare. (The duplicate
    /// case is real: styled-RETAIN diffs re-assert ops already present.)
    max_op_ever: Option<Arc<StyleOp>>,
}

#[derive(Debug, Clone)]
pub(super) struct RangeNumMapTrait;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Styles {
    pub(crate) styles: FxHashMap<StyleKey, StyleValue>,
}

impl Styles {
    pub(crate) fn has_key_value(&self, key: &str, value: &loro_common::LoroValue) -> bool {
        match self.get(&StyleKey::Key(key.into())) {
            Some(v) => match v.get() {
                Some(v) => &v.value == value,
                _ => false,
            },
            _ => false,
        }
    }

    /// Infer the anchors between the neighbor styles.
    /// Returns the last anchor of the left style and the first anchor of the right style.
    fn infer_anchors(&self, next: &Self) -> (Option<Arc<StyleOp>>, Option<Arc<StyleOp>>) {
        let mut left_anchor = None;
        let mut right_anchor = None;
        for (key, set) in self.styles.iter() {
            let right_value = next.styles.get(key);
            for diff in set
                .iter_ascending()
                .filter(|x| !right_value.is_some_and(|right| right.contains(x)))
            {
                assert!(left_anchor.is_none(), "left anchor should be unique");
                left_anchor = Some(diff.clone());
            }
        }

        for (key, set) in next.styles.iter() {
            let left_value = self.styles.get(key);
            for diff in set
                .iter_ascending()
                .filter(|x| !left_value.is_some_and(|left| left.contains(x)))
            {
                assert!(right_anchor.is_none(), "right anchor should be unique");
                right_anchor = Some(diff.clone());
            }
        }

        (left_anchor, right_anchor)
    }
}

impl Deref for Styles {
    type Target = FxHashMap<StyleKey, StyleValue>;

    fn deref(&self) -> &Self::Target {
        &self.styles
    }
}

impl DerefMut for Styles {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.styles
    }
}

pub(super) static EMPTY_STYLES: Lazy<Styles> = Lazy::new(Default::default);

#[derive(Debug, Clone)]
pub(crate) struct Elem {
    pub(crate) styles: Styles,
    pub(crate) len: usize,
}

/// §flowstate stylemap patch (A11.5 follow-on pass): a LAYERED op set —
/// a persistent shared `base` (`im::OrdSet`, O(1) clone, structural sharing)
/// plus a tiny owned `overlay` absorbing writes. The two failure modes this
/// reconciles, both measured on the styled-divergence repro:
/// * plain `BTreeSet` deep-copied the whole set on every boundary insert and
///   split (the edit-side quadratic constant, 5.2s);
/// * pure sharing (`Arc<BTreeSet>`, then `im::OrdSet`) made `annotate`'s
///   per-covered-elem insert pay COW/path-copy costs — import degraded 85×/12×
///   (an annotate legitimately walks EVERY covered elem; membership is
///   load-bearing per elem, so that walk only gets cheap per-visit work, and
///   `overlay.push` is O(1) where persistent insert was not).
/// The set semantics are exactly `base ∪ overlay` with `overlay ∩ base = ∅`
/// (enforced on insert); the overlay folds into the base past a small bound so
/// clones stay O(1)+ε. Pruning is NOT sound (`infer_anchors` set-differences
/// and `remove_style_scanning_backward`'s early-break walk need exact
/// membership), so this changes representation only, never the logical set.
#[derive(Clone, Debug, Default)]
pub(crate) struct StyleValue {
    // we need a set here because we need to calculate the intersection of styles when
    // users insert new text between two style sets
    /// `None` until the overlay first overflows: real-world sets (a few
    /// overlapping ops) live ENTIRELY in the inline overlay and never touch
    /// `im` — an empty `im::OrdSet` allocates a ~1KB root chunk, which showed
    /// up as +44% alloc on the whole-paragraph mark-application path.
    base: Option<StyleOpSet>,
    overlay: smallvec::SmallVec<[Arc<StyleOp>; 8]>,
    /// Cached `base.get_max()` — `im`'s spine walk is too hot for the
    /// per-covered-elem dedup gate in [`Self::insert`]. Invariant:
    /// `base_max == base.get_max().cloned()`.
    base_max: Option<Arc<StyleOp>>,
}

/// Shared empty set for the `base: None` read paths (never mutated).
static EMPTY_OP_SET: Lazy<StyleOpSet> = Lazy::new(StyleOpSet::new);

type StyleOpSet = im::OrdSet<Arc<StyleOp>>;

/// Overlay entries beyond this fold into the shared base (amortized O(log)).
const OVERLAY_BOUND: usize = 8;

impl PartialEq for StyleValue {
    fn eq(&self, other: &Self) -> bool {
        if self.len() != other.len() {
            return false;
        }
        // Pointer fast path: same-provenance shares compare O(overlay), which
        // also lets `Elem::can_merge` re-merge split fragments cheaply.
        if self.base_ref().ptr_eq(other.base_ref()) {
            return match (self.overlay.len(), other.overlay.len()) {
                (0, 0) => true,
                _ => {
                    self.overlay.iter().all(|op| other.overlay.contains(op))
                        && other.overlay.iter().all(|op| self.overlay.contains(op))
                }
            };
        }
        self.iter_ascending()
            .zip(other.iter_ascending())
            .all(|(a, b)| a == b)
    }
}

impl Eq for StyleValue {}

impl StyleValue {
    pub fn insert(&mut self, value: Arc<StyleOp>) {
        if self.overlay.contains(&value) {
            return;
        }
        // Dedup against the base only when the op COULD be there: anything
        // newer than the base's cached max cannot be a duplicate.
        if self.base_max.as_ref().is_some_and(|max| value <= *max)
            && self.base_ref().contains(&value)
        {
            return;
        }
        self.insert_unchecked(value);
    }

    /// Insert an op PROVEN absent from the logical set (the caller holds a
    /// freshness proof — see `StyleRangeMap::annotate`'s map-level gate).
    fn insert_unchecked(&mut self, value: Arc<StyleOp>) {
        self.overlay.push(value);
        if self.overlay.len() > OVERLAY_BOUND {
            self.normalize();
        }
    }

    /// Fold the overlay into the base. After this the overlay is empty and
    /// the base owns the full logical set.
    fn normalize(&mut self) {
        let base = self.base.get_or_insert_with(StyleOpSet::new);
        for op in self.overlay.drain(..) {
            if self.base_max.as_ref().is_none_or(|max| op > *max) {
                self.base_max = Some(op.clone());
            }
            base.insert(op);
        }
    }

    /// The base set for read paths (`None` reads as the shared empty set).
    fn base_ref(&self) -> &StyleOpSet {
        self.base.as_ref().unwrap_or(&EMPTY_OP_SET)
    }

    fn len(&self) -> usize {
        self.base_ref().len() + self.overlay.len()
    }

    pub fn get(&self) -> Option<&Arc<StyleOp>> {
        let base_max = self.base_max.as_ref();
        let overlay_max = self.overlay.iter().max();
        match (base_max, overlay_max) {
            (Some(a), Some(b)) => Some(if a >= b { a } else { b }),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        }
    }

    /// Ascending iteration over the LOGICAL set (base ∪ overlay).
    fn iter_ascending(&self) -> impl Iterator<Item = &Arc<StyleOp>> {
        let mut overlay: smallvec::SmallVec<[&Arc<StyleOp>; 8]> =
            self.overlay.iter().collect();
        overlay.sort_unstable();
        MergeAscending {
            base: self.base_ref().iter().peekable(),
            overlay,
            overlay_ix: 0,
        }
    }

    fn is_empty(&self) -> bool {
        self.overlay.is_empty() && self.base.as_ref().is_none_or(|base| base.is_empty())
    }

    fn contains(&self, op: &Arc<StyleOp>) -> bool {
        self.overlay.contains(op) || self.base_ref().contains(op)
    }

    fn remove_op(&mut self, op: &Arc<StyleOp>) -> bool {
        if let Some(ix) = self.overlay.iter().position(|x| x == op) {
            self.overlay.swap_remove(ix);
            return true;
        }
        if let Some(base) = self.base.as_mut() {
            if base.remove(op).is_some() {
                if self.base_max.as_ref().is_some_and(|max| max == op) {
                    self.base_max = base.get_max().cloned();
                }
                return true;
            }
        }
        false
    }

    /// Newest `(lamport, peer)` op present in BOTH logical sets — the visible
    /// value of the intersection, found WITHOUT materializing it. This is what
    /// the per-keystroke `get_styles_for_insert` actually needs (its result
    /// only ever feeds `StyleMeta`, which reads each key's max op).
    fn max_common(&self, other: &Self) -> Option<Arc<StyleOp>> {
        let mut best: Option<&Arc<StyleOp>> = None;
        for op in self.overlay.iter() {
            if best.is_none_or(|b| op > b) && other.contains(op) {
                best = Some(op);
            }
        }
        for op in other.overlay.iter() {
            if best.is_none_or(|b| op > b) && self.contains(op) {
                best = Some(op);
            }
        }
        if self.base_ref().ptr_eq(other.base_ref()) {
            if let Some(max) = self.base_max.as_ref() {
                if best.is_none_or(|b| max > b) {
                    best = Some(max);
                }
            }
        } else {
            // Descending double-walk to the first common base op.
            let mut left = self.base_ref().iter().rev().peekable();
            let mut right = other.base_ref().iter().rev().peekable();
            while let (Some(a), Some(b)) = (left.peek(), right.peek()) {
                if best.is_some_and(|best_op| best_op >= a) || best.is_some_and(|best_op| best_op >= b) {
                    break;
                }
                match a.cmp(b) {
                    std::cmp::Ordering::Equal => {
                        if best.is_none_or(|best_op| *a > best_op) {
                            best = Some(*a);
                        }
                        break;
                    }
                    std::cmp::Ordering::Greater => {
                        left.next();
                    }
                    std::cmp::Ordering::Less => {
                        right.next();
                    }
                }
            }
        }
        best.cloned()
    }

    /// `self ∩= right`, returning whether the result is non-empty. A mark's
    /// start/end boundary intersection equals one SIDE (the doc-comment proof
    /// on [`StyleRangeMap::insert`]: the differing ops are exactly the anchors
    /// at the boundary), so the subset fast paths reuse that side's storage
    /// (O(compare), zero copies) and the allocating rebuild only runs for
    /// boundaries where ops both start AND end.
    fn intersect_with(&mut self, right: &Self) -> bool {
        if self.base_ref().ptr_eq(right.base_ref())
            && self.overlay.is_empty()
            && right.overlay.is_empty()
        {
            return !self.is_empty();
        }
        // One O(n+m) ordered double-walk over the LOGICAL sets decides both
        // subset relations.
        let mut left_only = false;
        let mut right_only = false;
        {
            let mut left = self.iter_ascending().peekable();
            let mut right_iter = right.iter_ascending().peekable();
            loop {
                match (left.peek(), right_iter.peek()) {
                    (Some(a), Some(b)) => match a.cmp(b) {
                        std::cmp::Ordering::Equal => {
                            left.next();
                            right_iter.next();
                        }
                        std::cmp::Ordering::Less => {
                            left_only = true;
                            left.next();
                        }
                        std::cmp::Ordering::Greater => {
                            right_only = true;
                            right_iter.next();
                        }
                    },
                    (Some(_), None) => {
                        left_only = true;
                        break;
                    }
                    (None, Some(_)) => {
                        right_only = true;
                        break;
                    }
                    (None, None) => break,
                }
            }
        }
        if !right_only {
            // right ⊆ self: intersection IS right — O(1) persistent clone.
            *self = right.clone();
        } else if left_only {
            // Neither side contains the other — materialize the intersection.
            let mut result = StyleOpSet::new();
            {
                let mut left = self.iter_ascending().peekable();
                let mut right_iter = right.iter_ascending().peekable();
                while let (Some(a), Some(b)) = (left.peek(), right_iter.peek()) {
                    match a.cmp(b) {
                        std::cmp::Ordering::Equal => {
                            result.insert((*a).clone());
                            left.next();
                            right_iter.next();
                        }
                        std::cmp::Ordering::Less => {
                            left.next();
                        }
                        std::cmp::Ordering::Greater => {
                            right_iter.next();
                        }
                    }
                }
            }
            self.base_max = result.get_max().cloned();
            self.base = Some(result);
            self.overlay.clear();
        }
        // else: self ⊆ right — intersection is self, keep as-is.
        !self.is_empty()
    }
}

/// Ascending merge of a sorted persistent base and a small sorted overlay.
struct MergeAscending<'value> {
    base: std::iter::Peekable<im::ordset::Iter<'value, Arc<StyleOp>>>,
    overlay: smallvec::SmallVec<[&'value Arc<StyleOp>; 8]>,
    overlay_ix: usize,
}

impl<'value> Iterator for MergeAscending<'value> {
    type Item = &'value Arc<StyleOp>;

    fn next(&mut self) -> Option<Self::Item> {
        let overlay_next = self.overlay.get(self.overlay_ix).copied();
        match (self.base.peek(), overlay_next) {
            (Some(b), Some(o)) => {
                if *b <= o {
                    self.base.next()
                } else {
                    self.overlay_ix += 1;
                    Some(o)
                }
            }
            (Some(_), None) => self.base.next(),
            (None, Some(o)) => {
                self.overlay_ix += 1;
                Some(o)
            }
            (None, None) => None,
        }
    }
}

impl Default for StyleRangeMap {
    fn default() -> Self {
        Self::new()
    }
}

type YieldStyle<'a> = Option<&'a mut dyn FnMut(&Styles, usize)>;

impl StyleRangeMap {
    /// §flowstate style-map probe (diagnostic, env-gated `FLOWSTATE_STYLE_MAP_PROBE`):
    /// sampled census of the range map — element (boundary-fragment) count, total
    /// `StyleOp` set entries, and the largest single set. Distinguishes the two
    /// candidate blowups behind the styled-divergence quadratic: FRAGMENTATION
    /// (elems explode because adjacent same-visible-style elems differ by dead
    /// ops and `can_merge`'s deep set equality never fires) vs ACCUMULATION
    /// (individual sets explode). Sampled every 8192 calls; the O(elems) walk is
    /// paid only on samples, and never when the env flag is absent.
    fn probe_sample(&self, op: &'static str) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if !*ENABLED.get_or_init(|| std::env::var_os("FLOWSTATE_STYLE_MAP_PROBE").is_some()) {
            return;
        }
        static CALLS: AtomicU64 = AtomicU64::new(0);
        let call = CALLS.fetch_add(1, Ordering::Relaxed);
        if call % 8192 != 0 {
            return;
        }
        let mut elems = 0usize;
        let mut set_entries = 0usize;
        let mut max_set = 0usize;
        let mut styled_elems = 0usize;
        for elem in self.tree.iter() {
            elems += 1;
            let mut elem_entries = 0usize;
            for value in elem.styles.values() {
                elem_entries += value.len();
                max_set = max_set.max(value.len());
            }
            if elem_entries > 0 {
                styled_elems += 1;
            }
            set_entries += elem_entries;
        }
        eprintln!(
            "[flowstate-style-map] op={op} calls={call} elems={elems} styled_elems={styled_elems} set_entries={set_entries} max_set={max_set}"
        );
    }

    pub fn new() -> Self {
        let mut tree = BTree::new();
        tree.push(Elem {
            styles: Default::default(),
            len: usize::MAX / 4,
        });

        Self {
            tree,
            has_style: false,
            max_op_ever: None,
        }
    }

    pub fn annotate(
        &mut self,
        range: Range<usize>,
        style: Arc<StyleOp>,
        mut yield_style: YieldStyle,
    ) {
        self.probe_sample("annotate");
        let range = self.tree.range::<LengthFinder>(range);
        if range.is_none() {
            unreachable!();
        }

        self.has_style = true;
        let range = range.unwrap();
        // §flowstate stylemap patch: an annotate legitimately visits every
        // covered elem (per-elem membership is load-bearing), so per-visit
        // work must stay O(1) — `StyleValue::insert` lands in the small owned
        // overlay, never copy-on-writing the shared base.
        let style_key = style.get_style_key();
        let fresh = self.max_op_ever.as_ref().is_none_or(|max| style > *max);
        if fresh {
            self.max_op_ever = Some(style.clone());
        }
        self.tree
            .update(range.start.cursor..range.end.cursor, &mut |x| {
                if let Some(value) = x.styles.get_mut(&style_key) {
                    if fresh {
                        value.insert_unchecked(style.clone());
                    } else {
                        value.insert(style.clone());
                    }
                } else {
                    let mut value = StyleValue::default();
                    value.insert_unchecked(style.clone());
                    x.styles.insert(style_key.clone(), value);
                }

                if let Some(y) = yield_style.as_mut() {
                    y(&x.styles, x.len);
                }
                None
            });
    }

    /// Get the styles of the range. If the range is not in the same leaf, return None.
    pub(crate) fn get_styles_of_range(&self, range: Range<usize>) -> Option<&Styles> {
        if !self.has_style {
            return None;
        }

        let right = self
            .tree
            .query::<LengthFinder>(&(range.end - 1))
            .unwrap()
            .cursor;
        let left = self
            .tree
            .query::<LengthFinder>(&range.start)
            .unwrap()
            .cursor;
        if left.leaf == right.leaf {
            Some(&self.tree.get_elem(left.leaf).unwrap().styles)
        } else {
            None
        }
    }

    pub(crate) fn range_contains_key(&self, range: Range<usize>, key: &StyleKey) -> bool {
        if range.is_empty() || !self.has_style {
            return false;
        }

        let mut query = self.tree.query::<LengthFinder>(&range.start).unwrap();
        let mut pos = range.start;
        loop {
            let elem = self.tree.get_elem(query.cursor.leaf).unwrap();
            if elem.styles.contains_key(key) {
                return true;
            }

            let remaining_in_elem = elem.len - query.cursor.offset;
            let next_pos = pos + remaining_in_elem;
            if next_pos >= range.end {
                break;
            }

            match self.tree.next_elem(query.cursor) {
                Some(next_cursor) => {
                    pos = next_pos;
                    query.cursor = next_cursor;
                }
                None => break,
            }
        }

        false
    }

    /// Insert entities at `pos` with length of `len`
    ///
    /// # Internal
    ///
    /// When inserting new text, we need to calculate the StyleSet of the new text based on the StyleSet before and after the insertion position.
    /// (It should be the intersection of the StyleSet before and after). The proof is as follows:
    ///
    /// Suppose when inserting text at position pos, the style set at positions pos - 1 and pos are called leftStyleSet and rightStyleSet respectively.
    ///
    /// - If there is a style x that exists in leftStyleSet but not in rightStyleSet, it means that the position pos - 1 is the end anchor of x.
    ///   The newly inserted text is after the end anchor of x, so the StyleSet of the new text should not include this style.
    /// - If there is a style x that exists in rightStyleSet but not in leftStyleSet, it means that the position pos is the start anchor of x.
    ///   The newly inserted text is before the start anchor of x, so the StyleSet of the new text should not include this style.
    /// - If both leftStyleSet and rightStyleSet contain style x, it means that the newly inserted text is within the style range, so the StyleSet should include x.
    pub fn insert(&mut self, pos: usize, len: usize) -> &Styles {
        if !self.has_style {
            return &EMPTY_STYLES;
        }
        self.probe_sample("insert");

        if pos == 0 {
            self.tree.prepend(Elem {
                len,
                styles: Default::default(),
            });
            return &EMPTY_STYLES;
        }

        if pos as isize == *self.tree.root_cache() {
            self.tree.push(Elem {
                len,
                styles: Default::default(),
            });
            return &EMPTY_STYLES;
        }

        let right = self.tree.query::<LengthFinder>(&pos).unwrap().cursor;
        let left = self.tree.query::<LengthFinder>(&(pos - 1)).unwrap().cursor;
        if left.leaf == right.leaf {
            // left and right are in the same element, we can increase the length of the element directly
            self.tree.update_leaf(left.leaf, |x| {
                x.len += len;
                (true, None, None)
            });
            return &self.tree.get_elem(left.leaf).unwrap().styles;
        }

        // insert by the intersection of left styles and right styles
        // (§flowstate stylemap patch: the map clone is Arc bumps per key, and
        // `intersect_with` reuses one side's storage on the common
        // pure-start/pure-end boundary — see its doc comment.)
        let mut styles = self.tree.get_elem(left.leaf).unwrap().styles.clone();
        let right_styles = &self.tree.get_elem(right.leaf).unwrap().styles;
        styles.retain(|key, value| {
            if let Some(right_value) = right_styles.get(key) {
                return value.intersect_with(right_value);
            }

            false
        });

        let (target, _) = self.tree.insert_by_path(right, Elem { len, styles });
        &self.tree.get_elem(target.leaf).unwrap().styles
    }

    /// Return the style sets beside `index` and get the intersection of them.
    pub fn get_styles_for_insert(&self, index: usize) -> StyleMeta {
        if index == 0 || !self.has_style {
            return StyleMeta::default();
        }

        let left = self
            .tree
            .query::<LengthFinder>(&(index - 1))
            .unwrap()
            .cursor;
        let right = self.tree.shift_path_by_one_offset(left).unwrap();
        if left.leaf == right.leaf {
            let styles = &self.tree.get_elem(left.leaf).unwrap().styles;
            styles.into()
        } else {
            // §flowstate stylemap patch: this result only ever feeds
            // `StyleMeta` (per-key max op), so build it via `max_common` — a
            // descending double-walk per key, ZERO set materialization. This
            // is the per-keystroke path (`get_styles_at_entity_index_for_insert`).
            let left_styles = &self.tree.get_elem(left.leaf).unwrap().styles;
            let right_styles = &self.tree.get_elem(right.leaf).unwrap().styles;
            let mut meta = StyleMeta::default();
            for (key, left_value) in left_styles.iter() {
                if let Some(right_value) = right_styles.get(key) {
                    if let Some(op) = left_value.max_common(right_value) {
                        meta.insert(
                            key.key().clone(),
                            crate::delta::StyleMetaItem {
                                value: op.to_value(),
                                lamport: op.lamport,
                                peer: op.peer,
                            },
                        );
                    }
                }
            }

            meta
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (Range<usize>, &Styles)> + '_ {
        let mut index = 0;
        self.tree.iter().filter_map(move |elem| {
            let len = elem.len;
            let value = &elem.styles;
            let range = index..index + len;
            index += len;
            if elem.styles.is_empty() {
                return None;
            }

            Some((range, value))
        })
    }

    /// Update the styles from `pos` to the start of the document.
    fn update_styles_scanning_backward(
        &mut self,
        pos: usize,
        mut f: impl FnMut(&mut Elem) -> ControlFlow<()>,
    ) {
        let mut cursor = self.tree.query::<LengthFinder>(&pos).map(|x| x.cursor);
        while let Some(inner_cursor) = cursor {
            cursor = self.tree.prev_elem(inner_cursor);
            let node = self.tree.get_elem_mut(inner_cursor.leaf).unwrap();
            match f(node) {
                ControlFlow::Continue(_) => {}
                ControlFlow::Break(_) => {
                    break;
                }
            }
        }
    }

    pub(crate) fn iter_range(
        &self,
        range: impl RangeBounds<usize>,
    ) -> impl Iterator<Item = ElemSlice<'_, Elem>> + '_ {
        let start = match range.start_bound() {
            std::ops::Bound::Included(x) => *x,
            std::ops::Bound::Excluded(x) => *x + 1,
            std::ops::Bound::Unbounded => 0,
        };

        let end = match range.end_bound() {
            std::ops::Bound::Included(x) => *x + 1,
            std::ops::Bound::Excluded(x) => *x,
            std::ops::Bound::Unbounded => usize::MAX,
        };

        let start = self.tree.query::<LengthFinder>(&start).unwrap();
        let end = self.tree.query::<LengthFinder>(&end).unwrap();
        self.tree.iter_range(start.cursor..end.cursor)
    }

    /// Return the expected style anchors with their indexes.
    pub(super) fn iter_anchors(&self) -> impl Iterator<Item = IterAnchorItem> + '_ {
        let mut index = 0;
        let empty_styles = &EMPTY_STYLES;
        let mut last: Option<&Elem> = None;
        let mut vec = Vec::new();
        for cur in self.tree.iter() {
            let last_styles = last.map(|x| &x.styles).unwrap_or(empty_styles);
            let (left_anchor, right_anchor) = last_styles.infer_anchors(&cur.styles);
            if let Some(left) = left_anchor {
                vec.push(IterAnchorItem {
                    index: index - 1,
                    op: left.clone(),
                    anchor_type: AnchorType::End,
                });
            }
            if let Some(right) = right_anchor {
                vec.push(IterAnchorItem {
                    index,
                    op: right.clone(),
                    anchor_type: AnchorType::Start,
                });
            }

            last = Some(cur);
            index += cur.len;
        }

        let last_styles = last.map(|x| &x.styles).unwrap_or(empty_styles);
        let (left_anchor, right_anchor) = last_styles.infer_anchors(empty_styles);
        if let Some(left) = left_anchor {
            vec.push(IterAnchorItem {
                index: index - 1,
                op: left.clone(),
                anchor_type: AnchorType::End,
            });
        }
        if let Some(right) = right_anchor {
            vec.push(IterAnchorItem {
                index,
                op: right.clone(),
                anchor_type: AnchorType::Start,
            });
        }

        vec.into_iter()
    }

    /// Remove the style scanning backward, return the start_entity_index
    pub fn remove_style_scanning_backward(
        &mut self,
        to_remove: &Arc<StyleOp>,
        last_index: usize,
    ) -> usize {
        let mut removed_len = 0;
        let key = to_remove.get_style_key();
        self.update_styles_scanning_backward(last_index, |elem| {
            removed_len += elem.len;
            let styles = &mut elem.styles;
            let mut has_removed = false;
            if let Some(value) = styles.get_mut(&key) {
                has_removed = value.remove_op(to_remove);
                if value.is_empty() {
                    styles.remove(&key);
                }
            }

            if has_removed {
                ControlFlow::Continue(())
            } else {
                ControlFlow::Break(())
            }
        });

        last_index + 1 - removed_len
    }

    pub fn delete(&mut self, range: Range<usize>) {
        if !self.has_style {
            return;
        }

        let start = self.tree.query::<LengthFinder>(&range.start).unwrap();
        let end = self.tree.query::<LengthFinder>(&range.end).unwrap();
        if start.cursor.leaf == end.cursor.leaf {
            // delete in the same element
            self.tree.update_leaf(start.cursor.leaf, |x| {
                x.len -= range.len();
                (true, None, None)
            });
            return;
        }

        self.tree.drain(start..end);
    }

    pub(crate) fn has_style(&self) -> bool {
        self.has_style
    }
}

pub(super) struct IterAnchorItem {
    pub(super) index: usize,
    pub(super) op: Arc<StyleOp>,
    pub(super) anchor_type: AnchorType,
}

impl UseLengthFinder<RangeNumMapTrait> for RangeNumMapTrait {
    fn get_len(cache: &isize) -> usize {
        *cache as usize
    }
}

impl HasLength for Elem {
    fn rle_len(&self) -> usize {
        self.len
    }
}

impl Mergeable for Elem {
    fn can_merge(&self, rhs: &Self) -> bool {
        self.styles == rhs.styles || rhs.len == 0
    }

    fn merge_right(&mut self, rhs: &Self) {
        self.len += rhs.len
    }

    fn merge_left(&mut self, left: &Self) {
        self.len += left.len;
    }
}

impl Sliceable for Elem {
    fn _slice(&self, range: std::ops::Range<usize>) -> Self {
        let len = range.len();
        Elem {
            styles: self.styles.clone(),
            len,
        }
    }
}

impl TryInsert for Elem {
    fn try_insert(&mut self, _pos: usize, elem: Self) -> Result<(), Self>
    where
        Self: Sized,
    {
        if self.styles == elem.styles {
            self.len += elem.len;
            Ok(())
        } else {
            Err(elem)
        }
    }
}

impl CanRemove for Elem {
    fn can_remove(&self) -> bool {
        self.len == 0
    }
}

impl BTreeTrait for RangeNumMapTrait {
    type Elem = Elem;
    type Cache = isize;
    type CacheDiff = isize;
    const USE_DIFF: bool = true;

    fn calc_cache_internal(
        cache: &mut Self::Cache,
        caches: &[generic_btree::Child<Self>],
    ) -> isize {
        let new_cache = caches.iter().map(|c| c.cache).sum();
        let diff = new_cache - *cache;
        *cache = new_cache;
        diff
    }

    fn merge_cache_diff(diff1: &mut Self::CacheDiff, diff2: &Self::CacheDiff) {
        *diff1 += diff2;
    }

    fn apply_cache_diff(cache: &mut Self::Cache, diff: &Self::CacheDiff) {
        *cache += diff;
    }

    fn get_elem_cache(elem: &Self::Elem) -> Self::Cache {
        elem.len as isize
    }

    fn new_cache_to_diff(cache: &Self::Cache) -> Self::CacheDiff {
        *cache
    }

    fn sub_cache(cache_lhs: &Self::Cache, cache_rhs: &Self::Cache) -> Self::CacheDiff {
        *cache_lhs - *cache_rhs
    }
}

#[cfg(test)]
mod test {
    use loro_common::PeerID;

    use crate::{change::Lamport, container::richtext::TextStyleInfoFlag};

    use super::*;

    fn new_style(n: i32) -> Arc<StyleOp> {
        Arc::new(StyleOp {
            lamport: n as Lamport,
            peer: n as PeerID,
            cnt: n,
            key: n.to_string().into(),
            info: TextStyleInfoFlag::default(),
            value: loro_common::LoroValue::Bool(true),
        })
    }

    #[test]
    fn test_basic_insert() {
        let mut map = StyleRangeMap::default();
        map.annotate(1..10, new_style(1), None);
        {
            map.insert(0, 1);
            assert_eq!(map.iter().count(), 1);
            for (range, map) in map.iter() {
                assert_eq!(range, 2..11);
                assert_eq!(map.len(), 1);
            }
        }
        {
            map.insert(11, 1);
            assert_eq!(map.iter().count(), 1);
            for (range, map) in map.iter() {
                assert_eq!(range, 2..11);
                assert_eq!(map.len(), 1);
            }
        }
        {
            map.insert(10, 1);
            assert_eq!(map.iter().count(), 1);
            for (range, map) in map.iter() {
                assert_eq!(range, 2..12);
                assert_eq!(map.len(), 1);
            }
        }
    }

    #[test]
    fn delete_style() {
        let mut map = StyleRangeMap::default();
        map.annotate(1..10, new_style(1), None);
        {
            map.delete(0..2);
            assert_eq!(map.iter().count(), 1);
            for (range, map) in map.iter() {
                assert_eq!(range, 0..8);
                assert_eq!(map.len(), 1);
            }
        }
        {
            map.delete(2..4);
            for (range, map) in map.iter() {
                assert_eq!(range, 0..6);
                assert_eq!(map.len(), 1);
            }
            assert_eq!(map.iter().count(), 1);
        }
        {
            map.delete(6..8);
            assert_eq!(map.iter().count(), 1);
            for (range, map) in map.iter() {
                assert_eq!(range, 0..6);
                assert_eq!(map.len(), 1);
            }
        }
    }
}
