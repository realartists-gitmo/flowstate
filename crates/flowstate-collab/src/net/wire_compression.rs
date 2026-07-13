//! §perf: size-conditional zstd compression for the collab direct-transfer wire
//! payloads (full snapshots and incremental update batches). Two empirically
//! validated profiles, selected purely by payload size:
//!
//! - **Small payloads** (`< SMALL_PAYLOAD_MAX`: updates, small docs) → zstd with
//!   the shipped 256 KiB dictionary. A small CRDT delta shares structure with the
//!   dictionary corpus, giving ~2.52× (+27% over plain zstd).
//! - **Big payloads** (`>= SMALL_PAYLOAD_MAX`: full large-document snapshots) →
//!   zstd with long-distance matching and NO dictionary, giving ~2.96× with faster
//!   decode. The dictionary actively HURTS big files (~9% worse at L19), so it is
//!   off there.
//!
//! The chosen [`WireCodec`] plus the wire/uncompressed lengths ride on
//! [`DirectResponseHeader::Ok`](crate::proto_direct::DirectResponseHeader) so the
//! receiver decodes correctly and preallocates the decode buffer up front.
//!
//! The prepared encoder/decoder dictionaries are digested ONCE (lazy statics) and
//! reused for every payload — never re-digested per call.

use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use anyhow::{Context, Result, ensure};
use twox_hash::XxHash3_128;
use zstd::bulk::{Compressor, Decompressor};
use zstd::dict::{DecoderDictionary, EncoderDictionary};
use zstd::stream::raw::{CParameter, DParameter};

use crate::proto_direct::{MAX_PAYLOAD_LEN, WireCodec};

/// The 256 KiB dictionary shipped in-binary (built from a Flowstate-document
/// corpus — see the `flowstate-dictbuild` harness). Both peers embed the identical
/// bytes, so a `ZstdDict` frame one peer produces always decodes on the other.
static FLOWSTATE_DICT: &[u8] = include_bytes!("../../assets/flowstate-cards-256k.zdict");

/// The zstd dictionary id baked into [`FLOWSTATE_DICT`]. The dict path is only safe
/// while both peers ship this id; a peer with a different embedded dictionary would
/// emit frames this side cannot decode. Recorded here so a swapped dictionary is a
/// visible constant change (see the `dict_id_matches_shipped_dictionary` test).
pub const FLOWSTATE_DICT_ID: u32 = 1_296_305_636;

/// Payloads `>=` this size take the long-distance-matching profile (no dictionary);
/// smaller ones take the dictionary profile. ~4 MiB is the empirical crossover
/// where the dictionary stops helping and long-range matching starts to.
const SMALL_PAYLOAD_MAX: usize = 4 * 1024 * 1024;

/// Below this, zstd framing overhead outweighs any gain; stream verbatim.
const MIN_COMPRESS_LEN: usize = 1024;

/// Dictionary-profile level. Small payloads compress fast even at 19.
const DICT_LEVEL: i32 = 19;

/// Long-profile level for LIVE (uncached) serving. Level 3 sustains ~200 MB/s and
/// still beats plain L19 on big files, whereas L19 long compresses at only
/// ~3–7 MB/s — far too slow to compress a multi-hundred-MB snapshot per request.
/// Raise to 19 only behind a compress-once cache.
const LONG_LEVEL: i32 = 3;

/// Long-distance-matching window (2^27 = 128 MiB): large enough for big snapshots
/// while bounding decoder memory. The decoder must permit at least this window.
const LONG_WINDOW_LOG: u32 = 27;

/// Prepared once, reused for every payload — the CDict/DDict digest is never
/// recomputed per call.
static ENCODER_DICT: LazyLock<EncoderDictionary<'static>> = LazyLock::new(|| EncoderDictionary::copy(FLOWSTATE_DICT, DICT_LEVEL));
static DECODER_DICT: LazyLock<DecoderDictionary<'static>> = LazyLock::new(|| DecoderDictionary::copy(FLOWSTATE_DICT));

/// Whether the embedded dictionary's zstd id matches the recorded
/// [`FLOWSTATE_DICT_ID`]. Guards against a future edit swapping the `.zdict` file
/// without updating the constant: on mismatch the dictionary path is disabled
/// (small payloads fall back to the no-dictionary long profile) rather than
/// emitting `ZstdDict` frames that peers pinning the recorded id could not decode.
static DICT_ID_OK: LazyLock<bool> = LazyLock::new(|| {
  let id = zstd::zstd_safe::get_dict_id_from_dict(FLOWSTATE_DICT).map(std::num::NonZeroU32::get);
  let ok = id == Some(FLOWSTATE_DICT_ID);
  if !ok {
    tracing::error!(
      ?id,
      expected = FLOWSTATE_DICT_ID,
      "shipped zstd dictionary id does not match FLOWSTATE_DICT_ID; disabling the dictionary compression path"
    );
  }
  ok
});

/// Compressed-payload cache. Compressing a snapshot at `DICT_LEVEL` (19) is the one
/// slow step (~6 MB/s), and the exact same snapshot is re-served to every peer that
/// joins the same document version — so we compress once and reuse. Keyed by a
/// 128-bit content hash of the payload, which makes it self-invalidating: any edit
/// changes the snapshot bytes → new key → miss → recompress. A stale entry is simply
/// never hit again and ages out via LRU.
struct CacheEntry {
  key: u128,
  codec: WireCodec,
  bytes: Arc<[u8]>,
}

/// Small bound: a serving peer hosts a handful of live sessions, and we only need the
/// current version of each. Also capped by total bytes so a few big snapshots can't
/// balloon memory.
const CACHE_MAX_ENTRIES: usize = 8;
const CACHE_MAX_BYTES: usize = 512 * 1024 * 1024;

/// LRU by position: least-recently-used at the front, most-recent at the back.
static COMPRESSED_CACHE: LazyLock<Mutex<Vec<CacheEntry>>> = LazyLock::new(|| Mutex::new(Vec::new()));

fn lock_cache() -> MutexGuard<'static, Vec<CacheEntry>> {
  // A poisoned cache must never take down serving — recover the inner value.
  COMPRESSED_CACHE
    .lock()
    .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn cache_get(key: u128) -> Option<(WireCodec, Arc<[u8]>)> {
  let mut cache = lock_cache();
  let pos = cache.iter().position(|entry| entry.key == key)?;
  let entry = cache.remove(pos);
  let hit = (entry.codec, Arc::clone(&entry.bytes));
  cache.push(entry); // move to back = most-recently-used
  drop(cache); // release the lock before returning; the Arc keeps the bytes alive
  Some(hit)
}

fn cache_put(key: u128, codec: WireCodec, bytes: Arc<[u8]>) {
  let mut cache = lock_cache();
  if cache.iter().any(|entry| entry.key == key) {
    return; // a concurrent serve already inserted this version
  }
  cache.push(CacheEntry { key, codec, bytes });
  let mut total: usize = cache.iter().map(|entry| entry.bytes.len()).sum();
  while cache.len() > CACHE_MAX_ENTRIES || (cache.len() > 1 && total > CACHE_MAX_BYTES) {
    let evicted = cache.remove(0); // evict least-recently-used
    total -= evicted.bytes.len();
  }
  drop(cache); // release the lock promptly to minimize serve contention
}

/// §perf: an owned handle to the bytes to stream on the wire, without copying the
/// compressed payload. Either a slice borrowed from the caller's `payload` (verbatim
/// path) or a shared reference to the cached compressed buffer (same `Arc` the cache
/// holds). Replaces the old `Cow<[u8]>` return, which forced a full `.to_vec()` copy
/// on both the cache-hit and fresh-compress paths.
pub enum WireBytes<'a> {
  Borrowed(&'a [u8]),
  Shared(Arc<[u8]>),
}

impl WireBytes<'_> {
  #[must_use]
  pub fn as_slice(&self) -> &[u8] {
    match self {
      WireBytes::Borrowed(slice) => slice,
      WireBytes::Shared(shared) => shared,
    }
  }
}

/// Compress `payload` for the wire, choosing the codec by size. Returns the codec
/// and the bytes to stream: [`WireCodec::None`] borrowing `payload` verbatim when
/// compression is skipped (fast link, tiny payload) or fails to shrink; otherwise
/// the shared compressed bytes. Results are cached by content hash and the dictionary
/// is never re-digested.
#[must_use]
pub fn compress_for_wire(payload: &[u8], link_is_fast: bool) -> (WireCodec, WireBytes<'_>) {
  // On a very fast (LAN/localhost) link the decode (~700–900 MB/s) becomes the
  // ceiling, so compression buys nothing; and tiny payloads aren't worth framing.
  if link_is_fast || payload.len() < MIN_COMPRESS_LEN {
    return (WireCodec::None, WireBytes::Borrowed(payload));
  }
  // Hashing (xxh3, ~10 GB/s) is a rounding error next to compression, and a cache hit
  // skips the whole compress — the win that lets us keep level 19 without per-join cost.
  let key = XxHash3_128::oneshot(payload);
  if let Some((codec, bytes)) = cache_get(key) {
    // §perf: hand back the cached Arc directly instead of copying it out.
    return (codec, WireBytes::Shared(bytes));
  }
  let compressed = if payload.len() < SMALL_PAYLOAD_MAX && *DICT_ID_OK {
    compress_with_dict(payload).map(|bytes| (WireCodec::ZstdDict, bytes))
  } else {
    compress_long(payload).map(|bytes| (WireCodec::ZstdLong, bytes))
  };
  match compressed {
    // Only adopt compression when it actually shrank the payload.
    Ok((codec, bytes)) if bytes.len() < payload.len() => {
      // §perf: one Arc, shared by both the cache and the caller — no `.to_vec()` copy.
      let shared: Arc<[u8]> = Arc::from(bytes);
      cache_put(key, codec, Arc::clone(&shared));
      (codec, WireBytes::Shared(shared))
    },
    Ok(_) => (WireCodec::None, WireBytes::Borrowed(payload)),
    Err(error) => {
      tracing::warn!(error = %format_args!("{error:#}"), payload_bytes = payload.len(), "wire compression failed; sending payload uncompressed");
      (WireCodec::None, WireBytes::Borrowed(payload))
    },
  }
}

fn compress_with_dict(payload: &[u8]) -> Result<Vec<u8>> {
  let mut compressor = Compressor::with_prepared_dictionary(&ENCODER_DICT).context("preparing dictionary compressor")?;
  compressor
    .compress(payload)
    .context("zstd dictionary compression")
}

fn compress_long(payload: &[u8]) -> Result<Vec<u8>> {
  let mut compressor = Compressor::new(LONG_LEVEL).context("creating long-mode compressor")?;
  compressor
    .set_parameter(CParameter::EnableLongDistanceMatching(true))
    .context("enabling long-distance matching")?;
  compressor
    .set_parameter(CParameter::WindowLog(LONG_WINDOW_LOG))
    .context("setting window log")?;
  compressor
    .compress(payload)
    .context("zstd long-mode compression")
}

/// Decompress a wire payload streamed under `codec` back to `uncompressed_len`
/// bytes (which the sender declared in the response header). [`WireCodec::None`]
/// payloads are returned verbatim. Reuses the prepared decoder dictionary and
/// raises the decoder window for long frames.
pub fn decompress_from_wire(codec: WireCodec, wire: Vec<u8>, uncompressed_len: usize) -> Result<Vec<u8>> {
  ensure!(
    uncompressed_len <= MAX_PAYLOAD_LEN,
    "declared uncompressed length {uncompressed_len} exceeds {MAX_PAYLOAD_LEN} bytes"
  );
  let decoded = match codec {
    WireCodec::None => {
      ensure!(
        wire.len() == uncompressed_len,
        "uncompressed payload length {} does not match declared {uncompressed_len}",
        wire.len()
      );
      return Ok(wire);
    },
    WireCodec::ZstdDict => {
      let mut decompressor = Decompressor::with_prepared_dictionary(&DECODER_DICT).context("preparing dictionary decompressor")?;
      decompressor
        .decompress(&wire, uncompressed_len)
        .context("zstd dictionary decompression")?
    },
    WireCodec::ZstdLong => {
      let mut decompressor = Decompressor::new().context("creating long-mode decompressor")?;
      decompressor
        .set_parameter(DParameter::WindowLogMax(LONG_WINDOW_LOG))
        .context("raising decoder window for long frames")?;
      decompressor
        .decompress(&wire, uncompressed_len)
        .context("zstd long-mode decompression")?
    },
  };
  ensure!(
    decoded.len() == uncompressed_len,
    "decompressed length {} does not match declared {uncompressed_len}",
    decoded.len()
  );
  Ok(decoded)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn round_trip(payload: &[u8]) -> Vec<u8> {
    let (codec, wire) = compress_for_wire(payload, false);
    decompress_from_wire(codec, wire.as_slice().to_vec(), payload.len()).expect("decompress")
  }

  #[test]
  fn dict_id_matches_shipped_dictionary() {
    // Guards against a swapped dictionary file whose id would make our frames
    // undecodable on peers still shipping the recorded id.
    let id = zstd::zstd_safe::get_dict_id_from_dict(FLOWSTATE_DICT).map(std::num::NonZeroU32::get);
    assert_eq!(id, Some(FLOWSTATE_DICT_ID), "shipped dictionary id changed");
    assert!(*DICT_ID_OK, "dictionary compression path should be enabled");
  }

  #[test]
  fn small_payload_round_trips_via_dictionary() {
    let payload = b"{\"paragraph\":\"the quick brown fox\"}".repeat(64);
    assert!(payload.len() < SMALL_PAYLOAD_MAX);
    let (codec, _) = compress_for_wire(&payload, false);
    assert_eq!(codec, WireCodec::ZstdDict);
    assert_eq!(round_trip(&payload), payload);
  }

  #[test]
  fn big_payload_round_trips_via_long_no_dict() {
    // Compressible payload above the 4 MiB crossover so the long profile is chosen
    // and long-distance matching engages (~47 B block × 1024 × 100 ≈ 4.8 MiB).
    let block = b"paragraph metadata cursor boundary run styles ".repeat(1024);
    let payload = block.repeat(100);
    assert!(payload.len() >= SMALL_PAYLOAD_MAX);
    let (codec, wire) = compress_for_wire(&payload, false);
    assert_eq!(codec, WireCodec::ZstdLong);
    assert!(wire.as_slice().len() < payload.len());
    assert_eq!(round_trip(&payload), payload);
  }

  #[test]
  fn fast_link_and_tiny_payloads_skip_compression() {
    let payload = b"hello world".repeat(1024);
    assert_eq!(compress_for_wire(&payload, true).0, WireCodec::None);
    assert_eq!(compress_for_wire(b"tiny", false).0, WireCodec::None);
  }

  #[test]
  fn repeat_compression_is_consistent() {
    // The second call for the same content is served from the cache; it must return
    // the exact same codec + bytes (a corrupt cache would diverge here) and round-trip.
    let payload = b"{\"run\":\"cache probe content that is reasonably unique\"}".repeat(64);
    assert!(payload.len() >= MIN_COMPRESS_LEN && payload.len() < SMALL_PAYLOAD_MAX);
    let (codec1, wire1) = compress_for_wire(&payload, false);
    let (codec2, wire2) = compress_for_wire(&payload, false);
    assert_eq!(codec1, WireCodec::ZstdDict);
    assert_eq!(codec2, codec1);
    assert_eq!(wire1.as_slice(), wire2.as_slice(), "cached bytes must match freshly compressed bytes");
    assert_eq!(round_trip(&payload), payload);
  }

  #[test]
  fn cache_bounds_entries() {
    // Deterministic invariant (unaffected by parallel tests sharing the global cache):
    // overfilling must never leave more than the entry cap. Keys are placed high in the
    // space so they don't collide with content hashes other tests insert.
    let base: u128 = 0xC0DE << 112;
    for i in 0..(CACHE_MAX_ENTRIES as u128 + 4) {
      cache_put(base | i, WireCodec::ZstdLong, Arc::from(vec![0u8; 32]));
    }
    assert!(
      lock_cache().len() <= CACHE_MAX_ENTRIES,
      "eviction must bound the cache to CACHE_MAX_ENTRIES"
    );
  }

  #[test]
  fn incompressible_payload_falls_back_to_none() {
    // High-entropy xorshift bytes don't shrink under zstd → keep verbatim.
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let payload: Vec<u8> = (0..16_384)
      .map(|_| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state & 0xff) as u8
      })
      .collect();
    let (codec, wire) = compress_for_wire(&payload, false);
    assert_eq!(codec, WireCodec::None);
    assert_eq!(wire.as_slice(), payload.as_slice());
  }
}
