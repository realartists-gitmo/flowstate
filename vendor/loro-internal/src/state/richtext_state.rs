use generic_btree::{rle::HasLength, rle::Sliceable as _, Cursor};
use loro_common::{ContainerID, Counter, InternalString, LoroError, LoroResult, LoroValue, PeerID, ID};
use loro_delta::{DeltaRope, DeltaRopeBuilder};
use rustc_hash::{FxHashMap, FxHashSet};
use smallvec::SmallVec;
use std::ops::Range;
use std::sync::{Arc, Weak};

use crate::{
    container::{
        idx::ContainerIdx,
        list::list_op,
        richtext::{
            config::StyleConfigMap,
            richtext_state::{
                DrainInfo, EntityRangeInfo, IterRangeItem, PosType, RichtextStateChunk,
            },
            AnchorType, RichtextState as InnerState, StyleKey, StyleOp, Styles,
        },
    },
    delta::{StyleMeta, StyleMetaItem},
    event::{Diff, Index, InternalDiff, TextDiff},
    handler::TextDelta,
    op::{Op, RawOp},
    sync::RwLock,
    utils::{lazy::LazyLoad, string_slice::StringSlice},
    LoroDocInner,
};

use super::{ApplyLocalOpReturn, ContainerState, DiffApplyContext};

/// §perf-heaven T1: whether the `Src`-loader richtext-value fast path is active
/// (default ON; set `FLOWSTATE_RICHTEXT_NO_FASTPATH` to fall back to the full
/// `Src -> Dst` build). Read once and cached.
fn richtext_src_fastpath_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("FLOWSTATE_RICHTEXT_NO_FASTPATH").is_none())
}

/// §perf-heaven T1: whether to ALSO build the full state and assert the fast
/// path matches it (the fuzz-guided recovery guard). On in debug builds — where
/// the convergence/intent fuzz runs — and in any build via
/// `FLOWSTATE_RICHTEXT_VERIFY`, so `heaven.sh` can validate over the real corpus
/// in release. When on, `get_richtext_value` returns the BUILT value, so debug
/// behaviour is identical to before the patch regardless of the fast path.
fn richtext_verify_enabled() -> bool {
    static VERIFY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *VERIFY.get_or_init(|| cfg!(debug_assertions) || std::env::var_os("FLOWSTATE_RICHTEXT_VERIFY").is_some())
}

#[derive(Debug)]
pub struct RichtextState {
    idx: ContainerIdx,
    config: Arc<RwLock<StyleConfigMap>>,
    state: LazyLoad<RichtextStateLoader, InnerState>,
    /// This is used to indicate whether the richtext state is changed, so the downstream has an easy way to cache
    /// NOTE: We need to ensure the invariance that the version id is always increased when the richtext state is changed
    version_id: usize,
}

struct Pos {
    entity_index: usize,
    event_index: usize,
}

impl RichtextState {
    #[inline]
    pub fn new(idx: ContainerIdx, config: Arc<RwLock<StyleConfigMap>>) -> Self {
        Self {
            idx,
            config,
            state: LazyLoad::Src(Default::default()),
            version_id: 0,
        }
    }

    #[inline]
    fn update_version(&mut self) {
        self.version_id = self.version_id.wrapping_add(1);
    }

    /// Get the version id of the richtext
    ///
    /// This can be used to detect whether the richtext is changed
    #[inline]
    pub fn get_version_id(&self) -> usize {
        self.version_id
    }

    /// Get the text content of the richtext
    ///
    /// This uses `mut` because we may need to build the state from snapshot
    #[inline]
    pub fn to_string_mut(&mut self) -> String {
        self.state.get_mut().to_string()
    }

    #[allow(unused)]
    #[inline(always)]
    pub(crate) fn is_empty(&self) -> bool {
        match &self.state {
            LazyLoad::Src(s) => s.elements.is_empty(),
            LazyLoad::Dst(d) => d.is_empty(),
        }
    }

    pub(crate) fn diagnose(&self) {
        match &self.state {
            LazyLoad::Src(_) => {}
            LazyLoad::Dst(d) => d.diagnose(),
        }
    }

    #[allow(unused)]
    pub(crate) fn get_text_slice_by_event_index(
        &mut self,
        pos: usize,
        len: usize,
    ) -> LoroResult<String> {
        self.state.get_mut().get_text_slice_by_event_index(pos, len)
    }

    pub(crate) fn slice_delta(
        &mut self,
        start_index: usize,
        end_index: usize,
        pos_type: PosType,
    ) -> LoroResult<Vec<(String, StyleMeta)>> {
        self.state
            .get_mut()
            .slice_delta(start_index, end_index, pos_type)
    }

    #[allow(unused)]
    pub(crate) fn get_char_by_event_index(&mut self, pos: usize) -> Result<char, ()> {
        self.state.get_mut().get_char_by_event_index(pos)
    }

    /// §perf-heaven T1/T7.3: serve the char straight from the Src loader when
    /// the index space maps to unicode chunks, so object-anchor validation and
    /// event-index lookups don't force the `Src -> Dst` build. On non-wasm the
    /// EVENT index IS the unicode index, so `Event` reuses the same net-guarded
    /// walker as `Unicode` (guarded by the corpus Src-path equivalence net,
    /// T7.26). `Utf16` and wasm `Event` still fall through to the built state:
    /// there is no net exercising a utf16 chunk walk, so adding one would be
    /// unguarded dead code (the T1 trap).
    pub(crate) fn char_at_pos(&mut self, pos: usize, pos_type: PosType) -> Result<char, ()> {
        if let LazyLoad::Src(loader) = &self.state {
            let char_at = match pos_type {
                PosType::Unicode => Some(loader.char_at_unicode(pos)),
                PosType::Event => Some(if cfg!(feature = "wasm") {
                    loader.char_at_utf16(pos)
                } else {
                    loader.char_at_unicode(pos)
                }),
                PosType::Utf16 => Some(loader.char_at_utf16(pos)),
                _ => None,
            };
            if let Some(char_at) = char_at {
                return char_at.ok_or(());
            }
        }
        let event_pos = match pos_type {
            PosType::Event => pos,
            _ => self.index_to_event_index(pos, pos_type),
        };
        self.get_char_by_event_index(event_pos)
    }

    pub(crate) fn iter(&mut self, mut callback: impl FnMut(&str) -> bool) {
        for span in self.state.get_mut().iter_chunk() {
            match span {
                RichtextStateChunk::Text(text_chunk) => {
                    if !callback(text_chunk.as_str()) {
                        return;
                    }
                }
                RichtextStateChunk::Style { .. } => {}
            }
        }
    }

    pub(crate) fn iter_raw(&self, callback: &mut dyn FnMut(&RichtextStateChunk)) {
        let iter: &mut dyn Iterator<Item = &RichtextStateChunk>;
        let mut a;
        let mut b;
        match &self.state {
            LazyLoad::Src(s) => {
                a = Some(s.elements.iter());
                iter = &mut *a.as_mut().unwrap();
            }
            LazyLoad::Dst(s) => {
                b = Some(s.iter_chunk());
                iter = &mut *b.as_mut().unwrap();
            }
        }

        for c in iter {
            callback(c);
        }
    }

    fn get_style_start(
        &mut self,
        style_starts: &mut FxHashMap<Arc<StyleOp>, Pos>,
        style: &Arc<StyleOp>,
    ) -> Pos {
        self.update_version();
        match style_starts.remove(style) {
            Some(x) => x,
            None => {
                // this should happen rarely, so it should be fine to scan
                let mut pos = Pos {
                    entity_index: 0,
                    event_index: 0,
                };

                for c in self.state.get_mut().iter_chunk() {
                    match c {
                        RichtextStateChunk::Style {
                            style: s,
                            anchor_type: AnchorType::Start,
                        } if style == s => {
                            break;
                        }
                        RichtextStateChunk::Text(t) => {
                            pos.entity_index += t.unicode_len() as usize;
                            pos.event_index += t.event_len() as usize;
                        }
                        RichtextStateChunk::Style { .. } => {
                            pos.entity_index += 1;
                        }
                    }
                }
                pos
            }
        }
    }

    pub fn get_index_of_id(&self, id: ID) -> Option<usize> {
        let iter: &mut dyn Iterator<Item = &RichtextStateChunk>;
        let mut a;
        let mut b;
        match &self.state {
            LazyLoad::Src(s) => {
                a = Some(s.elements.iter());
                iter = &mut *a.as_mut().unwrap();
            }
            LazyLoad::Dst(s) => {
                b = Some(s.iter_chunk());
                iter = &mut *b.as_mut().unwrap();
            }
        }

        let mut index = 0;
        for elem in iter {
            let span = elem.get_id_span();
            if span.contains(id) {
                return Some(index + (id.counter - span.counter.start) as usize);
            }

            index += elem.rle_len();
        }

        None
    }

    pub fn get_text_index_of_id(&self, id: ID, use_event_index: bool) -> Option<usize> {
        let iter: &mut dyn Iterator<Item = &RichtextStateChunk>;
        let mut a;
        let mut b;
        match &self.state {
            LazyLoad::Src(s) => {
                a = Some(s.elements.iter());
                iter = &mut *a.as_mut().unwrap();
            }
            LazyLoad::Dst(s) => {
                b = Some(s.iter_chunk());
                iter = &mut *b.as_mut().unwrap();
            }
        }

        let mut index = 0;
        for elem in iter {
            let span = elem.get_id_span();
            if span.contains(id) {
                match elem {
                    RichtextStateChunk::Text(t) => {
                        if use_event_index {
                            let event_offset = t.convert_unicode_offset_to_event_offset(
                                (id.counter - span.counter.start) as usize,
                            );
                            return Some(index + event_offset);
                        } else {
                            return Some(index + (id.counter - span.counter.start) as usize);
                        }
                    }
                    RichtextStateChunk::Style { .. } => {
                        return Some(index);
                    }
                }
            }

            index += match elem {
                RichtextStateChunk::Text(t) => {
                    if use_event_index {
                        t.event_len() as usize
                    } else {
                        t.unicode_len() as usize
                    }
                }
                RichtextStateChunk::Style { .. } => 0,
            };
        }

        None
    }

    /// §perf (flowstate vendor patch): batch variant of [`Self::get_text_index_of_id`].
    /// Resolves the positions of many ids in a SINGLE pass over the chunks instead
    /// of O(elements) per id, so a caller resolving one id per record (e.g. a
    /// document projection resolving every paragraph boundary) becomes
    /// O(elements + ids·log ids) rather than O(elements · ids). `out[i]` is the
    /// position of `ids[i]`, or `None` if that id is not present in the current
    /// state (deleted) — callers that need history-traced resolution for those
    /// should fall back to the per-id `get_cursor_pos`. Each resolved entry is
    /// bit-identical to calling [`Self::get_text_index_of_id`] on that id.
    pub fn get_text_indices_of_ids(&self, ids: &[ID], use_event_index: bool) -> Vec<Option<usize>> {
        let mut out = vec![None; ids.len()];
        if ids.is_empty() {
            return out;
        }
        // Group query slots by peer, each sorted by counter, so a chunk's
        // contiguous (forward) id-span selects its matching queries by binary
        // search — keeping the whole resolution to a single chunk pass even when
        // every id shares one peer.
        let mut by_peer: FxHashMap<PeerID, Vec<(Counter, usize)>> = FxHashMap::default();
        for (i, id) in ids.iter().enumerate() {
            by_peer.entry(id.peer).or_default().push((id.counter, i));
        }
        for slots in by_peer.values_mut() {
            slots.sort_unstable_by_key(|(counter, _)| *counter);
        }
        let mut remaining = ids.len();

        let iter: &mut dyn Iterator<Item = &RichtextStateChunk>;
        let mut a;
        let mut b;
        match &self.state {
            LazyLoad::Src(s) => {
                a = Some(s.elements.iter());
                iter = &mut *a.as_mut().unwrap();
            }
            LazyLoad::Dst(s) => {
                b = Some(s.iter_chunk());
                iter = &mut *b.as_mut().unwrap();
            }
        }

        let mut index = 0;
        for elem in iter {
            if remaining == 0 {
                break;
            }
            let span = elem.get_id_span();
            if let Some(slots) = by_peer.get(&span.peer) {
                // Chunk id-spans are always forward (start < end), matching the
                // `span.contains(id)` window `start <= counter < end` used by the
                // per-id path above. Select that counter window by binary search.
                let start = span.counter.start;
                let end = span.counter.end;
                let lo = slots.partition_point(|(counter, _)| *counter < start);
                for &(counter, slot) in &slots[lo..] {
                    if counter >= end {
                        break;
                    }
                    if out[slot].is_none() {
                        let pos = match elem {
                            RichtextStateChunk::Text(t) => {
                                if use_event_index {
                                    index + t.convert_unicode_offset_to_event_offset((counter - start) as usize)
                                } else {
                                    index + (counter - start) as usize
                                }
                            }
                            RichtextStateChunk::Style { .. } => index,
                        };
                        out[slot] = Some(pos);
                        remaining -= 1;
                    }
                }
            }
            index += match elem {
                RichtextStateChunk::Text(t) => {
                    if use_event_index {
                        t.event_len() as usize
                    } else {
                        t.unicode_len() as usize
                    }
                }
                RichtextStateChunk::Style { .. } => 0,
            };
        }

        out
    }

    pub(crate) fn get_delta(&mut self) -> Vec<TextDelta> {
        let mut delta = Vec::new();
        for span in self.state.get_mut().iter() {
            let next_attr = span.attributes.to_option_map();
            match delta.last_mut() {
                Some(TextDelta::Insert { insert, attributes }) if &next_attr == attributes => {
                    insert.push_str(span.text.as_str());
                    continue;
                }
                _ => {}
            }
            delta.push(TextDelta::Insert {
                insert: span.text.as_str().to_string(),
                attributes: next_attr,
            })
        }
        delta
    }
}

#[cfg(test)]
mod tests {
    use crate::{container::richtext::StyleKey, cursor::PosType, handler::HandlerTrait, LoroDoc};

    #[test]
    fn has_style_key_in_entity_range_basic() {
        let loro = LoroDoc::new_auto_commit();
        let text = loro.get_text("text");
        text.insert(0, "abcdef", PosType::Unicode).unwrap();
        text.mark(1, 3, "bold", true.into(), PosType::Unicode)
            .unwrap();

        let bold_key = StyleKey::Key("bold".into());
        let has_style = text
            .with_state(|state| {
                let st = state.as_richtext_state_mut().unwrap();
                let (entity_range, _) =
                    st.get_entity_range_and_styles_at_range(1..3, PosType::Unicode);
                Ok(st.has_style_key_in_entity_range(entity_range, &bold_key))
            })
            .unwrap();
        assert!(has_style);

        let missing = text
            .with_state(|state| {
                let st = state.as_richtext_state_mut().unwrap();
                let (entity_range, _) =
                    st.get_entity_range_and_styles_at_range(4..5, PosType::Unicode);
                Ok(st.has_style_key_in_entity_range(entity_range, &bold_key))
            })
            .unwrap();
        assert!(!missing);
    }

    #[test]
    fn has_style_key_in_entity_range_spans_elements() {
        let loro = LoroDoc::new_auto_commit();
        let text = loro.get_text("text");
        text.insert(0, "abcdefgh", PosType::Unicode).unwrap();
        text.mark(0, 2, "bold", true.into(), PosType::Unicode)
            .unwrap();
        text.mark(3, 5, "bold", true.into(), PosType::Unicode)
            .unwrap();

        let bold_key = StyleKey::Key("bold".into());

        let has_style_across_segments = text
            .with_state(|state| {
                let st = state.as_richtext_state_mut().unwrap();
                let (entity_range, _) =
                    st.get_entity_range_and_styles_at_range(0..5, PosType::Unicode);
                Ok(st.has_style_key_in_entity_range(entity_range, &bold_key))
            })
            .unwrap();
        assert!(has_style_across_segments);

        let gap_has_style = text
            .with_state(|state| {
                let st = state.as_richtext_state_mut().unwrap();
                let (entity_range, _) =
                    st.get_entity_range_and_styles_at_range(6..7, PosType::Unicode);
                Ok(st.has_style_key_in_entity_range(entity_range, &bold_key))
            })
            .unwrap();
        assert!(!gap_has_style);
    }
}

impl Clone for RichtextState {
    fn clone(&self) -> Self {
        Self {
            idx: self.idx,
            config: self.config.clone(),
            state: self.state.clone(),
            version_id: self.version_id,
        }
    }
}

impl ContainerState for RichtextState {
    fn container_idx(&self) -> ContainerIdx {
        self.idx
    }

    fn is_state_empty(&self) -> bool {
        match &self.state {
            LazyLoad::Src(s) => s.is_empty(),
            LazyLoad::Dst(s) => s.is_empty(),
        }
    }

    // TODO: refactor
    fn apply_diff_and_convert(&mut self, diff: InternalDiff, _ctx: DiffApplyContext) -> Diff {
        self.update_version();
        let InternalDiff::RichtextRaw(richtext) = diff else {
            unreachable!()
        };

        // tracing::info!("Self state = {:#?}", &self);
        // PERF: compose delta
        let mut ans: TextDiff = TextDiff::new();
        let mut style_delta: TextDiff = TextDiff::new();
        let mut style_starts: FxHashMap<Arc<StyleOp>, Pos> = FxHashMap::default();
        let mut entity_index = 0;
        let mut event_index = 0;
        let mut new_style_deltas: Vec<TextDiff> = Vec::new();
        for span in richtext.iter() {
            match span {
                loro_delta::DeltaItem::Retain { len, .. } => {
                    entity_index += len;
                }
                loro_delta::DeltaItem::Replace { value, delete, .. } => {
                    if *delete > 0 {
                        // Deletions
                        let mut deleted_style_keys: FxHashSet<InternalString> =
                            FxHashSet::default();
                        let DrainInfo {
                            start_event_index: start,
                            end_event_index: end,
                            affected_style_range,
                        } = self.state.get_mut().drain_by_entity_index(
                            entity_index,
                            *delete,
                            Some(&mut |c| match c {
                                RichtextStateChunk::Style {
                                    style,
                                    anchor_type: AnchorType::Start,
                                } => {
                                    deleted_style_keys.insert(style.key.clone());
                                }
                                RichtextStateChunk::Style {
                                    style,
                                    anchor_type: AnchorType::End,
                                } => {
                                    deleted_style_keys.insert(style.key.clone());
                                }
                                _ => {}
                            }),
                        );

                        if start > event_index {
                            ans.push_retain(start - event_index, Default::default());
                            event_index = start;
                        }

                        if let Some((entity_range, event_range)) = affected_style_range {
                            let mut delta: TextDiff = DeltaRopeBuilder::new()
                                .retain(event_range.start, Default::default())
                                .build();
                            let mut entity_len_sum = 0;
                            let expected_sum = entity_range.len();

                            for IterRangeItem {
                                event_len,
                                chunk,
                                styles,
                                entity_len,
                                ..
                            } in self.state.get_mut().iter_range(entity_range)
                            {
                                entity_len_sum += entity_len;
                                match chunk {
                                    RichtextStateChunk::Text(_) => {
                                        let mut style_meta: StyleMeta = styles.into();
                                        for key in deleted_style_keys.iter() {
                                            if !style_meta.contains_key(key) {
                                                style_meta.insert(
                                                    key.clone(),
                                                    StyleMetaItem {
                                                        lamport: 0,
                                                        peer: 0,
                                                        value: LoroValue::Null,
                                                    },
                                                )
                                            }
                                        }
                                        delta.push_retain(
                                            event_len,
                                            style_meta.to_option_map().unwrap_or_default().into(),
                                        );
                                    }
                                    RichtextStateChunk::Style { .. } => {}
                                }
                            }

                            debug_assert_eq!(entity_len_sum, expected_sum);
                            delta.chop();
                            style_delta.compose(&delta);
                        }

                        ans.push_delete(end - start);
                    }

                    if value.rle_len() > 0 {
                        // Insertions
                        match value {
                            RichtextStateChunk::Text(s) => {
                                let (pos, styles) =
                                    self.state.get_mut().insert_elem_at_entity_index(
                                        entity_index,
                                        RichtextStateChunk::Text(s.clone()),
                                    );
                                // PERF: this can be optimized
                                let insert_styles = Into::<StyleMeta>::into(styles.clone())
                                    .to_option_map()
                                    .unwrap_or_default();

                                if pos > event_index {
                                    ans.push_retain(pos - event_index, Default::default());
                                }
                                event_index = pos + s.event_len() as usize;
                                ans.push_insert(
                                    StringSlice::from(s.bytes().clone()),
                                    insert_styles.into(),
                                );
                            }
                            RichtextStateChunk::Style { anchor_type, style } => {
                                let (new_event_index, _) =
                                    self.state.get_mut().insert_elem_at_entity_index(
                                        entity_index,
                                        RichtextStateChunk::Style {
                                            style: style.clone(),
                                            anchor_type: *anchor_type,
                                        },
                                    );

                                if new_event_index > event_index {
                                    ans.push_retain(
                                        new_event_index - event_index,
                                        Default::default(),
                                    );
                                    // inserting style anchor will not affect event_index's positions
                                    event_index = new_event_index;
                                }

                                match anchor_type {
                                    AnchorType::Start => {
                                        style_starts.insert(
                                            style.clone(),
                                            Pos {
                                                entity_index,
                                                event_index: new_event_index,
                                            },
                                        );
                                    }
                                    AnchorType::End => {
                                        // get the pair of style anchor. now we can annotate the range
                                        let Pos {
                                            entity_index: start_entity_index,
                                            event_index: start_event_index,
                                        } = self.get_style_start(&mut style_starts, style);
                                        let mut delta: TextDiff = DeltaRopeBuilder::new()
                                            .retain(start_event_index, Default::default())
                                            .build();
                                        // we need to + 1 because we also need to annotate the end anchor
                                        let event =
                                            self.state.get_mut().annotate_style_range_with_event(
                                                start_entity_index..entity_index + 1,
                                                style.clone(),
                                            );
                                        for (s, l) in event {
                                            delta.push_retain(
                                                l,
                                                s.to_option_map().unwrap_or_default().into(),
                                            );
                                        }

                                        delta.chop();
                                        new_style_deltas.push(delta);
                                    }
                                }
                            }
                        }

                        entity_index += value.rle_len();
                    }
                }
            }
        }

        for s in new_style_deltas {
            style_delta.compose(&s);
        }
        // self.check_consistency_between_content_and_style_ranges();
        ans.compose(&style_delta);
        Diff::Text(ans)
    }

    fn apply_diff(&mut self, diff: InternalDiff, _ctx: DiffApplyContext) -> LoroResult<()> {
        self.update_version();
        let InternalDiff::RichtextRaw(richtext) = diff else {
            unreachable!()
        };

        if let LazyLoad::Src(loader) = &mut self.state {
            if loader.try_apply_append_delta(&richtext) {
                return Ok(());
            }
        }

        // Fast path for plain-text deltas (no style anchors / style ranges).
        //
        // Rebuilding avoids repeated BTree queries and mutations when the delta is very "choppy"
        // (many small edit spans), but it allocates and clones chunks, so it can be slower for
        // small deltas. Use a cheap cost model to enable it only when it's likely beneficial.
        let should_fast_apply = {
            #[inline]
            fn ilog2_ceil(x: usize) -> usize {
                debug_assert!(x > 0);
                (usize::BITS - (x - 1).leading_zeros()) as usize
            }

            let state = self.state.get_mut();
            if state.has_styles() {
                false
            } else {
                // `edit_actions` approximates how many BTree mutations the incremental path will do:
                // each Replace with delete>0 becomes a drain, and each Replace with value>0 becomes an insert.
                let mut edit_actions: usize = 0;
                let mut is_plain_text_delta = true;
                for span in richtext.iter() {
                    match span {
                        loro_delta::DeltaItem::Retain { .. } => {}
                        loro_delta::DeltaItem::Replace { value, delete, .. } => {
                            if *delete > 0 {
                                edit_actions += 1;
                            }
                            if value.rle_len() > 0 {
                                if !matches!(value, RichtextStateChunk::Text(_)) {
                                    is_plain_text_delta = false;
                                    break;
                                }
                                edit_actions += 1;
                            }
                        }
                    }
                }

                if !is_plain_text_delta || edit_actions == 0 {
                    false
                } else {
                    let content_nodes = state.content_node_len().max(1);
                    let log_n = ilog2_ceil(content_nodes + 1).max(1);
                    let incremental_score = edit_actions.saturating_mul(log_n);
                    let rebuild_score = content_nodes.saturating_add(edit_actions);

                    let old_len = richtext.old_len().max(1);
                    let avg_action_span = old_len / edit_actions;
                    // A very rough proxy for "choppiness": many edit actions with small average span.
                    // The thresholds are intentionally conservative to avoid rebuilding for small or
                    // localized deltas.
                    let is_choppy = edit_actions >= 256 && avg_action_span <= 32;

                    is_choppy && incremental_score >= rebuild_score.saturating_mul(4)
                }
            }
        };

        if should_fast_apply {
            let new_state = {
                let state = self.state.get_mut();
                let mut chunks: Vec<RichtextStateChunk> = Vec::new();

                let mut src_iter = state.iter_chunk();
                let mut cur = src_iter.next();
                let mut cur_offset: usize = 0;

                for span in richtext.iter() {
                    match span {
                        loro_delta::DeltaItem::Retain { len, .. } => {
                            let mut left = *len;
                            while left > 0 {
                                let chunk = cur.expect("Delta retain exceeds source length");
                                let chunk_len = chunk.rle_len();
                                if chunk_len == 0 {
                                    cur = src_iter.next();
                                    cur_offset = 0;
                                    continue;
                                }
                                let available = chunk_len - cur_offset;
                                let take = left.min(available);
                                if take == chunk_len && cur_offset == 0 {
                                    chunks.push(chunk.clone());
                                } else {
                                    chunks.push(chunk.slice(cur_offset..cur_offset + take));
                                }

                                left -= take;
                                cur_offset += take;
                                if cur_offset == chunk_len {
                                    cur = src_iter.next();
                                    cur_offset = 0;
                                }
                            }
                        }
                        loro_delta::DeltaItem::Replace { value, delete, .. } => {
                            let mut left = *delete;
                            while left > 0 {
                                let chunk = cur.expect("Delta delete exceeds source length");
                                let chunk_len = chunk.rle_len();
                                if chunk_len == 0 {
                                    cur = src_iter.next();
                                    cur_offset = 0;
                                    continue;
                                }
                                let available = chunk_len - cur_offset;
                                let take = left.min(available);
                                left -= take;
                                cur_offset += take;
                                if cur_offset == chunk_len {
                                    cur = src_iter.next();
                                    cur_offset = 0;
                                }
                            }

                            if value.rle_len() > 0 {
                                chunks.push(value.clone());
                            }
                        }
                    }
                }

                if let Some(chunk) = cur {
                    let chunk_len = chunk.rle_len();
                    if cur_offset < chunk_len {
                        if cur_offset == 0 {
                            chunks.push(chunk.clone());
                        } else {
                            chunks.push(chunk.slice(cur_offset..chunk_len));
                        }
                    }
                }
                for chunk in src_iter {
                    chunks.push(chunk.clone());
                }

                InnerState::from_chunks(chunks.into_iter())
            };

            *self.state.get_mut() = new_state;
            return Ok(());
        }

        let mut style_starts: FxHashMap<Arc<StyleOp>, usize> = FxHashMap::default();
        let mut entity_index = 0;
        for span in richtext.iter() {
            match span {
                loro_delta::DeltaItem::Retain { len, .. } => {
                    entity_index += len;
                }
                loro_delta::DeltaItem::Replace { value, delete, .. } => {
                    if *delete > 0 {
                        // Deletions
                        self.state
                            .get_mut()
                            .drain_by_entity_index(entity_index, *delete, None);
                    }
                    if value.rle_len() > 0 {
                        // Insertions
                        match value {
                            RichtextStateChunk::Text(s) => {
                                self.state.get_mut().insert_elem_at_entity_index(
                                    entity_index,
                                    RichtextStateChunk::Text(s.clone()),
                                );
                            }
                            RichtextStateChunk::Style { style, anchor_type } => {
                                self.state.get_mut().insert_elem_at_entity_index(
                                    entity_index,
                                    RichtextStateChunk::Style {
                                        style: style.clone(),
                                        anchor_type: *anchor_type,
                                    },
                                );

                                if *anchor_type == AnchorType::Start {
                                    style_starts.insert(style.clone(), entity_index);
                                } else {
                                    let start_pos = match style_starts.get(style) {
                                        Some(x) => *x,
                                        None => {
                                            // This should be rare, so it should be fine to scan
                                            let mut start_entity_index = 0;
                                            for c in self.state.get_mut().iter_chunk() {
                                                match c {
                                                    RichtextStateChunk::Style {
                                                        style: s,
                                                        anchor_type: AnchorType::Start,
                                                    } if style == s => {
                                                        break;
                                                    }
                                                    RichtextStateChunk::Text(t) => {
                                                        start_entity_index +=
                                                            t.unicode_len() as usize;
                                                    }
                                                    RichtextStateChunk::Style { .. } => {
                                                        start_entity_index += 1;
                                                    }
                                                }
                                            }
                                            start_entity_index
                                        }
                                    };
                                    // we need to + 1 because we also need to annotate the end anchor
                                    self.state.get_mut().annotate_style_range(
                                        start_pos..entity_index + 1,
                                        style.clone(),
                                    );
                                }
                            }
                        }
                        entity_index += value.rle_len();
                    }
                }
            }
        }

        // self.check_consistency_between_content_and_style_ranges()
        Ok(())
    }

    fn apply_local_op(&mut self, r_op: &RawOp, op: &Op) -> LoroResult<ApplyLocalOpReturn> {
        self.update_version();
        match &op.content {
            crate::op::InnerContent::List(l) => match l {
                list_op::InnerListOp::Insert { slice: _, pos: _ } => {
                    unreachable!()
                }
                list_op::InnerListOp::InsertText {
                    slice,
                    unicode_len: _,
                    unicode_start: _,
                    pos,
                } => {
                    self.state.get_mut().insert_at_entity_index(
                        *pos as usize,
                        slice.clone(),
                        r_op.id_full(),
                    );
                }
                list_op::InnerListOp::Delete(del) => {
                    self.state.get_mut().drain_by_entity_index(
                        del.start() as usize,
                        rle::HasLength::atom_len(&del),
                        None,
                    );
                }
                list_op::InnerListOp::StyleStart {
                    start,
                    end,
                    key,
                    value,
                    info,
                } => {
                    // Behavior here is a little different from apply_diff.
                    //
                    // When apply_diff, we only do the mark when we have included both
                    // StyleStart and StyleEnd.
                    //
                    // When applying local op, we can do the mark when we have StyleStart.
                    // We can assume StyleStart and StyleEnd are always appear in a pair
                    // for apply_local_op. (Because for local behavior, when we mark,
                    // we always create a pair of style ops.)
                    self.state.get_mut().mark_with_entity_index(
                        *start as usize..*end as usize,
                        Arc::new(StyleOp {
                            lamport: r_op.lamport,
                            peer: r_op.id.peer,
                            cnt: r_op.id.counter,
                            key: key.clone(),
                            value: value.clone(),
                            info: *info,
                        }),
                    );
                }
                list_op::InnerListOp::Set { .. } => {}
                list_op::InnerListOp::StyleEnd => {}
                list_op::InnerListOp::Move { .. } => unreachable!(),
            },
            _ => unreachable!(),
        }

        // self.check_consistency_between_content_and_style_ranges();
        Ok(Default::default())
    }

    fn to_diff(&mut self, _doc: &Weak<LoroDocInner>) -> Diff {
        let mut delta = TextDiff::new();
        for span in self.state.get_mut().iter() {
            delta.push_insert(
                span.text,
                span.attributes.to_option_map().unwrap_or_default().into(),
            );
        }

        Diff::Text(delta)
    }

    // value is a list
    fn get_value(&mut self) -> LoroValue {
        match &self.state {
            LazyLoad::Src(loader) => loader.to_plain_string().into(),
            LazyLoad::Dst(_) => self.state.get_mut().to_string().into(),
        }
    }

    #[doc = r" Get the index of the child container"]
    #[allow(unused)]
    fn get_child_index(&self, id: &ContainerID) -> Option<Index> {
        None
    }

    #[allow(unused)]
    fn get_child_containers(&self) -> Vec<ContainerID> {
        Vec::new()
    }

    fn contains_child(&self, _id: &ContainerID) -> bool {
        false
    }

    fn fork(&self, config: &crate::configure::Configure) -> Self {
        Self {
            idx: self.idx,
            config: config.text_style_config.clone(),
            state: self.state.clone(),
            version_id: 0,
        }
    }
}

impl RichtextState {
    #[inline(always)]
    pub fn len_utf8(&mut self) -> usize {
        // §perf-heaven T8.15: O(1) from the Src loader (net-guarded accumulator).
        if let LazyLoad::Src(loader) = &self.state {
            return loader.bytes_len;
        }
        self.state.get_mut().len_utf8()
    }

    #[inline(always)]
    pub fn len(&mut self, pos_type: PosType) -> usize {
        // §perf-heaven T1: unicode/entity length from the Src loader (O(1)) so the
        // `char_at` bounds check (`len(Unicode)`) does not force the `Src -> Dst`
        // build ahead of the object-anchor validation fast path.
        if let LazyLoad::Src(loader) = &self.state {
            match pos_type {
                PosType::Unicode => return loader.unicode_len,
                PosType::Entity => return loader.entity_index,
                PosType::Utf16 => return loader.utf16_len,
                PosType::Event => {
                    return if cfg!(feature = "wasm") { loader.utf16_len } else { loader.unicode_len };
                },
                PosType::Bytes => return loader.bytes_len,
                #[allow(unreachable_patterns)]
                _ => {},
            }
        }
        self.state.get_mut().len(pos_type)
    }

    #[inline(always)]
    pub fn len_utf16(&mut self) -> usize {
        // §perf-heaven T7.2: O(1) from the Src loader (net-guarded accumulator).
        if let LazyLoad::Src(loader) = &self.state {
            return loader.utf16_len;
        }
        self.state.get_mut().len_utf16()
    }

    #[inline(always)]
    pub fn len_entity(&self) -> usize {
        match &self.state {
            LazyLoad::Src(s) => s.entity_index,
            LazyLoad::Dst(d) => d.len_entity(),
        }
    }

    pub fn len_event(&mut self) -> usize {
        if cfg!(feature = "wasm") {
            self.len_utf16()
        } else {
            self.len_unicode()
        }
    }

    #[inline]
    #[allow(dead_code)]
    pub(crate) fn has_styles(&mut self) -> bool {
        self.state.get_mut().has_styles()
    }

    pub(crate) fn has_style_key_in_entity_range(
        &mut self,
        range: Range<usize>,
        key: &StyleKey,
    ) -> bool {
        self.state.get_mut().range_has_style_key(range, key)
    }

    /// Check if the content and style ranges are consistent.
    ///
    /// Panic if inconsistent.
    #[allow(unused)]
    pub(crate) fn check_consistency_between_content_and_style_ranges(&mut self) {
        if !cfg!(debug_assertions) {
            return;
        }

        self.state
            .get_mut()
            .check_consistency_between_content_and_style_ranges();
    }

    #[inline]
    pub fn len_unicode(&mut self) -> usize {
        // §perf-heaven T1: answer from the Src loader (O(1)) instead of forcing
        // the `Src -> Dst` B-tree build. The projection calls this per boundary
        // BEFORE `to_delta`; forcing the build here would defeat the whole
        // cold-decode fast path.
        if let LazyLoad::Src(loader) = &self.state {
            return loader.unicode_len;
        }
        self.state.get_mut().len_unicode()
    }

    #[inline]
    pub(crate) fn get_entity_index_for_text_insert(
        &mut self,
        index: usize,
        pos_type: PosType,
    ) -> Result<(usize, Option<Cursor>), LoroError> {
        self.state
            .get_mut()
            .get_entity_index_for_text_insert(index, pos_type)
    }

    #[inline]
    pub(crate) fn get_event_index_by_cursor(&mut self, cursor: Cursor) -> usize {
        self.state
            .get_mut()
            .get_index_from_cursor(cursor, PosType::Event)
            .unwrap()
    }

    pub(crate) fn get_entity_range_and_styles_at_range(
        &mut self,
        range: Range<usize>,
        pos_type: PosType,
    ) -> (Range<usize>, Option<&Styles>) {
        self.state
            .get_mut()
            .get_entity_range_and_text_styles_at_range(range, pos_type)
    }

    #[inline]
    pub(crate) fn get_styles_at_entity_index(&mut self, entity_index: usize) -> StyleMeta {
        self.state
            .get_mut()
            .get_styles_at_entity_index_for_insert(entity_index)
    }

    #[inline]
    pub(crate) fn get_text_entity_ranges_in_event_index_range(
        &mut self,
        pos: usize,
        len: usize,
    ) -> LoroResult<Vec<EntityRangeInfo>> {
        self.state
            .get_mut()
            .get_text_entity_ranges(pos, len, PosType::Event)
    }

    #[inline]
    pub fn get_richtext_value(&mut self) -> LoroValue {
        // §perf-heaven T1: on a freshly decoded snapshot the state is still
        // `LazyLoad::Src` (a cheap loader of chunks + resolved style ranges).
        // `get_mut()` here would force the whole O(doc) `Src -> Dst` B-tree build
        // — the dominant cost of the cold projection body decode. Resolve the
        // value directly from the loader instead, skipping the text tree. Debug
        // (and `FLOWSTATE_RICHTEXT_VERIFY`) builds ALSO build the tree and assert
        // the fast path is bit-identical, then return the built value — so the
        // fuzz/corpus oracle validates the cut and a divergence trips loudly.
        let fast = if richtext_src_fastpath_enabled() {
            match &self.state {
                LazyLoad::Src(loader) => Some(InnerState::richtext_value_from_src(&loader.elements, &loader.style_ranges)),
                LazyLoad::Dst(_) => None,
            }
        } else {
            None
        };
        match fast {
            Some(fast) if !richtext_verify_enabled() => fast,
            Some(fast) => {
                let slow = self.state.get_mut().get_richtext_value();
                assert!(
                    fast == slow,
                    "FLOWSTATE perf-heaven T1: richtext_value_from_src diverged from the built richtext state"
                );
                slow
            }
            None => self.state.get_mut().get_richtext_value(),
        }
    }

    /// §act-eleven A11.10 (flowstate vendor patch): stream `(text, styles)`
    /// spans to `f` without materializing the delta `Vec<LoroValue>`. Mirrors
    /// [`Self::get_richtext_value`]'s T1 discipline exactly: on a freshly
    /// decoded snapshot (`LazyLoad::Src`) the spans come straight from the
    /// loader without forcing the O(doc) B-tree build; verify builds take the
    /// value-level equivalence assert first (same oracle — both value functions
    /// are expressed on these walkers), then stream from the built state.
    pub fn for_each_richtext_span(&mut self, f: &mut dyn FnMut(&str, &crate::delta::StyleMeta)) {
        if richtext_src_fastpath_enabled() {
            if let LazyLoad::Src(loader) = &self.state {
                if !richtext_verify_enabled() {
                    InnerState::for_each_span_from_src(&loader.elements, &loader.style_ranges, f);
                    return;
                }
                let fast =
                    InnerState::richtext_value_from_src(&loader.elements, &loader.style_ranges);
                let slow = self.state.get_mut().get_richtext_value();
                assert!(
                    fast == slow,
                    "FLOWSTATE act-eleven A11.10: for_each_span_from_src diverged from the built richtext state"
                );
                // fall through to the built state below
            }
        }
        self.state.get_mut().for_each_richtext_span(f);
    }

    #[inline]
    pub(crate) fn get_stable_position(
        &mut self,
        event_index: usize,
        get_by_event_index: bool,
    ) -> Option<ID> {
        self.state.get_mut().get_stable_position_at_event_index(
            event_index,
            if get_by_event_index {
                PosType::Event
            } else {
                PosType::Unicode
            },
        )
    }

    pub(crate) fn entity_index_to_event_index(&mut self, entity_index: usize) -> usize {
        self.state
            .get_mut()
            .entity_index_to_event_index(entity_index)
    }

    pub(crate) fn index_to_event_index(&mut self, index: usize, pos_type: PosType) -> usize {
        self.state.get_mut().index_to_event_index(index, pos_type)
    }

    pub(crate) fn event_index_to_unicode_index(&mut self, event_index: usize) -> usize {
        self.state
            .get_mut()
            .event_index_to_unicode_index(event_index)
    }
}

#[derive(Debug, Default, Clone)]
pub(crate) struct RichtextStateLoader {
    start_anchor_pos: FxHashMap<ID, usize>,
    elements: SmallVec<[RichtextStateChunk; 1]>,
    style_ranges: Vec<(Arc<StyleOp>, Range<usize>)>,
    entity_index: usize,
    /// §perf-heaven T1: running unicode length of the pushed text chunks, so
    /// `RichtextState::len_unicode` can answer O(1) from the loader WITHOUT
    /// forcing the `Src -> Dst` B-tree build. The projection's boundary resolver
    /// calls `len_unicode` per boundary; if that forced the build, the whole
    /// cold-decode fast path would be defeated before `to_delta` ever runs.
    unicode_len: usize,
    /// §perf-heaven T7.2: running UTF-16 length, mirroring `unicode_len`, so
    /// `len_utf16` / `len(Utf16)` / `len(Event)` (and the wasm event index) can
    /// also answer O(1) from the loader. Both accumulators are validated against
    /// the built B-tree in `into_state` (a debug-assert net), which also
    /// retroactively guards the T1 `unicode_len` field.
    utf16_len: usize,
    /// §perf-heaven T8.15: running UTF-8 BYTE length, so `len_utf8` / `len(Bytes)`
    /// also answer O(1) from the loader instead of forcing the `Src -> Dst` build.
    /// Validated against the built B-tree in `into_state`.
    bytes_len: usize,
}

impl From<RichtextStateLoader> for InnerState {
    fn from(value: RichtextStateLoader) -> Self {
        value.into_state()
    }
}

impl RichtextStateLoader {
    /// §perf-heaven T1: the char at a UNICODE offset, straight from the Src
    /// chunks (no `Src -> Dst` build). Backs `char_at_pos` on a still-loaded
    /// snapshot; guarded by the corpus Src-path equivalence net (T7.26).
    fn char_at_unicode(&self, pos: usize) -> Option<char> {
        let mut remaining = pos;
        for elem in &self.elements {
            if let RichtextStateChunk::Text(chunk) = elem {
                let chunk_len = chunk.unicode_len() as usize;
                if remaining < chunk_len {
                    return chunk.as_str().chars().nth(remaining);
                }
                remaining -= chunk_len;
            }
        }
        None
    }

    /// §perf-heaven T8.16: the char at a UTF-16 offset, straight from the Src
    /// chunks (mirrors `char_at_unicode`, walking by `utf16_len`). Returns `None`
    /// for an out-of-range OR mid-surrogate offset (a position inside a surrogate
    /// pair is not a char boundary), matching the built state's `char_at`. Backs
    /// the wasm event-index / `Utf16` `char_at` so they don't force the `Dst`
    /// build. Guarded by the same `utf16_len` net as `len_utf16` (`into_state`).
    fn char_at_utf16(&self, pos: usize) -> Option<char> {
        let mut remaining = pos;
        for elem in &self.elements {
            if let RichtextStateChunk::Text(chunk) = elem {
                let chunk_len = chunk.utf16_len() as usize;
                if remaining < chunk_len {
                    let mut acc = 0usize;
                    for ch in chunk.as_str().chars() {
                        if acc == remaining {
                            return Some(ch);
                        }
                        acc += ch.len_utf16();
                        if acc > remaining {
                            return None; // `pos` fell inside a surrogate pair
                        }
                    }
                    return None;
                }
                remaining -= chunk_len;
            }
        }
        None
    }

    pub fn push(&mut self, elem: RichtextStateChunk) {
        if let RichtextStateChunk::Style { style, anchor_type } = &elem {
            if *anchor_type == AnchorType::Start {
                self.start_anchor_pos
                    .insert(ID::new(style.peer, style.cnt), self.entity_index);
            } else {
                let start_pos = self
                    .start_anchor_pos
                    .remove(&ID::new(style.peer, style.cnt))
                    .expect("Style start not found");

                // we need to + 1 because we also need to annotate the end anchor
                self.style_ranges
                    .push((style.clone(), start_pos..self.entity_index + 1));
            }
        }

        if let RichtextStateChunk::Text(text) = &elem {
            self.unicode_len += text.unicode_len() as usize;
            self.utf16_len += text.utf16_len() as usize;
            self.bytes_len += text.bytes().len();
        }
        self.entity_index += elem.rle_len();
        self.elements.push(elem);
    }

    pub fn into_state(self) -> InnerState {
        // §perf-heaven T7.2 net: capture the running accumulators BEFORE the
        // elements are consumed, then assert they equal the built B-tree's real
        // lengths. This is the oracle for the Src-safe `len_unicode`/`len_utf16`
        // /`len_utf8` fast paths — if `push` ever mis-accumulates, the very act
        // of building the state (any `Src -> Dst` promotion) trips here.
        let expected_unicode = self.unicode_len;
        let expected_utf16 = self.utf16_len;
        let expected_bytes = self.bytes_len;
        let mut state = InnerState::from_chunks(self.elements.into_iter());
        for (style, range) in self.style_ranges {
            state.annotate_style_range(range, style);
        }

        if cfg!(debug_assertions) {
            debug_assert_eq!(
                state.len_unicode(),
                expected_unicode,
                "T7 loader unicode_len accumulation diverged from the built B-tree"
            );
            debug_assert_eq!(
                state.len_utf16(),
                expected_utf16,
                "T7.2 loader utf16_len accumulation diverged from the built B-tree"
            );
            debug_assert_eq!(
                state.len_utf8(),
                expected_bytes,
                "T8.15 loader bytes_len accumulation diverged from the built B-tree"
            );
            state.check();
        }

        state
    }

    fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }

    fn to_plain_string(&self) -> String {
        let len = self
            .elements
            .iter()
            .map(|elem| match elem {
                RichtextStateChunk::Text(text) => text.bytes().len(),
                RichtextStateChunk::Style { .. } => 0,
            })
            .sum();
        let mut text = String::with_capacity(len);
        for elem in &self.elements {
            if let RichtextStateChunk::Text(chunk) = elem {
                text.push_str(chunk.as_str());
            }
        }
        text
    }

    fn try_apply_append_delta(&mut self, delta: &DeltaRope<RichtextStateChunk, ()>) -> bool {
        // Style anchors/ranges need InnerState's range maintenance.
        if !self.start_anchor_pos.is_empty() || !self.style_ranges.is_empty() {
            return false;
        }

        let old_len = self.entity_index;
        let mut current_len = self.entity_index;
        let mut entity_index = 0;
        let mut appended = Vec::new();
        for span in delta.iter() {
            match span {
                loro_delta::DeltaItem::Retain { len, .. } => {
                    entity_index += len;
                    if entity_index > old_len {
                        return false;
                    }
                }
                loro_delta::DeltaItem::Replace { value, delete, .. } => {
                    if *delete > 0 {
                        return false;
                    }

                    let insert_len = value.rle_len();
                    if insert_len == 0 {
                        continue;
                    }

                    if !matches!(value, RichtextStateChunk::Text(_)) {
                        return false;
                    }

                    if entity_index != current_len {
                        return false;
                    }

                    appended.push(value.clone());
                    entity_index += insert_len;
                    current_len += insert_len;
                }
            }
        }

        if appended.is_empty() {
            return false;
        }

        for elem in appended {
            self.push(elem);
        }
        true
    }
}

mod snapshot {
    use loro_common::{IdFull, InternalString, LoroValue, PeerID};
    use rustc_hash::FxHashMap;
    use serde_columnar::columnar;
    use std::sync::Arc;

    use crate::{
        container::richtext::{
            self, richtext_state::RichtextStateChunk, str_slice::StrSlice, StyleOp,
            TextStyleInfoFlag,
        },
        encoding::value_register::ValueRegister,
        state::{
            decode_lamport_from_delta, decode_peer_from_table, decode_peer_table,
            state_decode_error, ContainerCreationContext, ContainerState, FastStateSnapshot,
        },
        utils::lazy::LazyLoad,
    };

    use super::{RichtextState, RichtextStateLoader};

    #[columnar(vec, ser, de, iterable)]
    #[derive(Debug, Clone)]
    struct EncodedTextSpan {
        #[columnar(strategy = "DeltaRle")]
        peer_idx: usize,
        #[columnar(strategy = "DeltaRle")]
        counter: i32,
        #[columnar(strategy = "DeltaRle")]
        lamport_sub_counter: i32,
        /// positive for text
        /// 0 for mark start
        /// -1 for mark end
        #[columnar(strategy = "DeltaRle")]
        len: i32,
    }

    #[columnar(vec, ser, de, iterable)]
    #[derive(Debug, Clone)]
    struct EncodedMark {
        key_idx: usize,
        value: LoroValue,
        info: u8,
    }

    #[columnar(ser, de)]
    struct EncodedText {
        #[columnar(class = "vec", iter = "EncodedTextSpan")]
        spans: Vec<EncodedTextSpan>,
        keys: Vec<InternalString>,
        marks: Vec<EncodedMark>,
    }

    impl FastStateSnapshot for RichtextState {
        /// Encodes the RichtextState into a compact binary format for fast snapshot storage and retrieval.
        ///
        /// The encoding format consists of:
        /// 1. The full text content as a string, encoded using postcard serialization.
        /// 2. A series of EncodedTextSpan structs representing text chunks and style markers:
        ///    - peer_idx: Index of the peer ID in a value register (delta-RLE encoded)
        ///    - counter: Operation counter (delta-RLE encoded)
        ///    - lamport_sub_counter: Lamport timestamp - counter (delta-RLE encoded)
        ///    - len: Length of text chunk or marker type (-1 for end, 0 for start, positive for text)
        /// 3. A list of unique style keys as InternalString.
        /// 4. A series of EncodedMark structs for style information:
        ///    - key_idx: Index of the style key in the keys list
        ///    - value: The style value
        ///    - info: Additional style information as a byte
        fn encode_snapshot_fast<W: std::io::prelude::Write>(&mut self, mut w: W) {
            let value = self.get_value().into_string().unwrap();
            postcard::to_io(&*value, &mut w).unwrap();
            let mut spans = Vec::new();
            let mut marks = Vec::new();

            let mut peers: ValueRegister<PeerID> = ValueRegister::new();
            let iter: &mut dyn Iterator<Item = &RichtextStateChunk>;
            let mut a;
            let mut b;
            match &self.state {
                LazyLoad::Src(s) => {
                    a = Some(s.elements.iter());
                    iter = &mut *a.as_mut().unwrap();
                }
                LazyLoad::Dst(s) => {
                    b = Some(s.iter_chunk());
                    iter = &mut *b.as_mut().unwrap();
                }
            }

            let mut keys: ValueRegister<InternalString> = ValueRegister::new();

            for chunk in iter {
                match chunk {
                    RichtextStateChunk::Text(t) => {
                        let id = t.id_full();
                        assert!(t.unicode_len() > 0);
                        spans.push(EncodedTextSpan {
                            peer_idx: peers.register(&id.peer),
                            counter: id.counter,
                            lamport_sub_counter: id.lamport as i32 - id.counter,
                            len: t.unicode_len(),
                        })
                    }
                    RichtextStateChunk::Style { style, anchor_type } => match anchor_type {
                        richtext::AnchorType::Start => {
                            let id = style.id_full();
                            spans.push(EncodedTextSpan {
                                peer_idx: peers.register(&id.peer),
                                counter: id.counter,
                                lamport_sub_counter: id.lamport as i32 - id.counter,
                                len: 0,
                            });
                            marks.push(EncodedMark {
                                key_idx: keys.register(&style.key),
                                value: style.value.clone(),
                                info: style.info.to_byte(),
                            })
                        }
                        richtext::AnchorType::End => {
                            let id = style.id_full();
                            spans.push(EncodedTextSpan {
                                peer_idx: peers.register(&id.peer),
                                counter: id.counter + 1,
                                lamport_sub_counter: id.lamport as i32 - id.counter,
                                len: -1,
                            })
                        }
                    },
                }
            }

            let peers = peers.unwrap_vec();
            leb128::write::unsigned(&mut w, peers.len() as u64).unwrap();
            for peer in peers {
                w.write_all(&peer.to_le_bytes()).unwrap();
            }

            let bytes = serde_columnar::to_vec(&EncodedText {
                spans,
                keys: keys.unwrap_vec(),
                marks,
            })
            .unwrap();
            w.write_all(&bytes).unwrap();
        }

        fn decode_value(bytes: &[u8]) -> loro_common::LoroResult<(loro_common::LoroValue, &[u8])> {
            let (value, bytes) = postcard::take_from_bytes(bytes).map_err(|_| {
                loro_common::LoroError::DecodeError(
                    "Decode list value failed".to_string().into_boxed_str(),
                )
            })?;
            let s: String = value;
            Ok((LoroValue::String(s.into()), bytes))
        }

        fn decode_snapshot_fast(
            idx: crate::container::idx::ContainerIdx,
            (string, mut bytes): (loro_common::LoroValue, &[u8]),
            ctx: ContainerCreationContext,
        ) -> loro_common::LoroResult<Self>
        where
            Self: Sized,
        {
            let mut text = RichtextState::new(idx, ctx.configure.text_style_config.clone());
            let mut loader = RichtextStateLoader::default();
            let peers = decode_peer_table(&mut bytes, "Decode richtext state failed")?;

            let string = string.into_string().map_err(|_| {
                state_decode_error("Decode richtext state failed: value is not a string")
            })?;
            let mut s = StrSlice::new_from_str(&string);
            let iters = serde_columnar::from_bytes::<EncodedText>(bytes).map_err(|err| {
                state_decode_error(format!(
                    "Decode richtext state failed: invalid spans: {err}"
                ))
            })?;
            let keys = iters.keys;
            let span_iter = iters.spans.into_iter();
            let mut mark_iter = iters.marks.into_iter();
            let mut id_to_style = FxHashMap::default();
            for span in span_iter {
                let EncodedTextSpan {
                    peer_idx,
                    counter,
                    lamport_sub_counter,
                    len,
                } = span;
                let peer =
                    decode_peer_from_table(&peers, peer_idx, "Decode richtext state failed")?;
                let lamport = decode_lamport_from_delta(
                    counter,
                    lamport_sub_counter,
                    "Decode richtext state failed",
                )?;
                let id_full = IdFull::new(peer, counter, lamport);
                let chunk = match len {
                    0 => {
                        // Style Start
                        let EncodedMark {
                            key_idx,
                            value,
                            info,
                        } = mark_iter.next().ok_or_else(|| {
                            state_decode_error("Decode richtext state failed: missing style mark")
                        })?;
                        let key = keys.get(key_idx).ok_or_else(|| {
                            state_decode_error(
                                "Decode richtext state failed: style key index out of range",
                            )
                        })?;
                        let style_op = Arc::new(StyleOp {
                            lamport,
                            peer: id_full.peer,
                            cnt: id_full.counter,
                            key: key.clone(),
                            value,
                            info: TextStyleInfoFlag::from_byte(info),
                        });
                        id_to_style.insert(id_full.id(), style_op.clone());
                        RichtextStateChunk::new_style(style_op, richtext::AnchorType::Start)
                    }
                    -1 => {
                        // Style End
                        let style = id_to_style.remove(&id_full.id().inc(-1)).ok_or_else(|| {
                            state_decode_error("Decode richtext state failed: unmatched style end")
                        })?;
                        RichtextStateChunk::new_style(style, richtext::AnchorType::End)
                    }
                    len => {
                        if len < -1 {
                            return Err(state_decode_error(
                                "Decode richtext state failed: invalid text span length",
                            ));
                        }

                        // Text
                        if s.as_str().chars().count() < len as usize {
                            return Err(state_decode_error(
                                "Decode richtext state failed: text span exceeds value length",
                            ));
                        }

                        let (new, rest) = s.split_at_unicode_pos(len as usize);
                        s = rest;
                        RichtextStateChunk::new_text(new.bytes().clone(), id_full)
                    }
                };

                loader.push(chunk);
            }
            if !s.as_str().is_empty() {
                return Err(state_decode_error(
                    "Decode richtext state failed: text value not fully covered",
                ));
            }
            if mark_iter.next().is_some() {
                return Err(state_decode_error(
                    "Decode richtext state failed: unused style mark",
                ));
            }
            if !id_to_style.is_empty() {
                return Err(state_decode_error(
                    "Decode richtext state failed: unclosed style mark",
                ));
            }
            text.state = LazyLoad::Src(loader);
            // NOTE: We need to ensure the invariance that the version id is always increased when the richtext state is changed
            // This is used to avoid the version_id to be the same as the previous zero version
            text.version_id = 1;
            Ok(text)
        }
    }

    #[cfg(test)]
    mod tests {
        use loro_common::{ContainerType, LoroValue};

        use crate::container::idx::ContainerIdx;

        use super::*;

        #[test]
        fn richtext_fast_snapshot_rejects_corrupt_state_metadata() {
            let idx = ContainerIdx::from_index_and_type(0, ContainerType::Text);
            let configure = Default::default();
            let ctx = ContainerCreationContext {
                configure: &configure,
                peer: 0,
            };

            assert!(RichtextState::decode_snapshot_fast(
                idx,
                (LoroValue::String("a".into()), &[1]),
                ctx,
            )
            .is_err());

            let mut text = RichtextState::new(idx, configure.text_style_config.clone());
            let mut bytes = Vec::new();
            text.encode_snapshot_fast(&mut bytes);
            let (_, state_bytes) = RichtextState::decode_value(&bytes).unwrap();
            assert!(RichtextState::decode_snapshot_fast(
                idx,
                (LoroValue::String("a".into()), state_bytes),
                ctx,
            )
            .is_err());
        }
    }
}
