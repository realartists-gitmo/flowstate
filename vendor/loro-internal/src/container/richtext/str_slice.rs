use append_only_bytes::{AppendOnlyBytes, BytesSlice};

/// It's just a wrapper around [BytesSlice] that makes sure
/// the content of the bytes is a valid utf-8 string
pub(crate) struct StrSlice(BytesSlice);

impl StrSlice {
    #[allow(unused)]
    pub fn new(bytes: BytesSlice) -> Option<Self> {
        let _str = std::str::from_utf8(&bytes).ok()?;
        Some(Self(bytes))
    }

    pub fn new_from_str(str: &str) -> Self {
        let mut a = AppendOnlyBytes::new();
        a.push_str(str);
        Self(a.slice(..))
    }

    pub fn bytes(&self) -> &BytesSlice {
        &self.0
    }

    pub fn as_str(&self) -> &str {
        // SAFETY: We ensure that the content is always valid utf8
        unsafe { std::str::from_utf8_unchecked(&self.0) }
    }

    /// §flowstate vendor patch (oom-leads #2): split at `pos` unicode chars,
    /// or `None` when the slice holds FEWER than `pos` chars — one walk of at
    /// most `pos + 1` chars. Replaces `decode_snapshot_fast`'s per-span
    /// `chars().count()` bounds check over the ENTIRE remaining document
    /// string, which made snapshot decode O(spans × chars) — 84% of a
    /// 2.6M-char document's cold open.
    pub fn try_split_at_unicode_pos(&self, pos: usize) -> Option<(Self, Self)> {
        let s = self.as_str();
        let mut split_at = None;
        let mut chars_seen = 0usize;
        for (u, (i, _)) in s.char_indices().enumerate() {
            if u == pos {
                split_at = Some(i);
                break;
            }
            chars_seen = u + 1;
        }
        let split_at = match split_at {
            Some(byte_ix) => byte_ix,
            None => {
                if chars_seen < pos {
                    return None;
                }
                self.0.len()
            }
        };
        Some((
            Self(self.0.slice_clone(..split_at)),
            Self(self.0.slice_clone(split_at..)),
        ))
    }

    #[allow(dead_code, reason = "upstream API kept verbatim; flowstate callers use try_split_at_unicode_pos")]
    pub fn split_at_unicode_pos(&self, pos: usize) -> (Self, Self) {
        let s = self.as_str();
        let mut split_at = self.0.len();
        for (u, (i, _)) in s.char_indices().enumerate() {
            if u == pos {
                split_at = i;
                break;
            }
        }

        (
            Self(self.0.slice_clone(..split_at)),
            Self(self.0.slice_clone(split_at..)),
        )
    }
}
