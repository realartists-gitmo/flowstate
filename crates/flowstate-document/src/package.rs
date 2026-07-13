use std::{
  collections::HashSet,
  fs::{self, OpenOptions},
  io::{self, Cursor, Read as _, Seek as _, SeekFrom, Write as _},
  path::Path,
};

use flowstate_fidelity::{self as fidelity, FidelityClass};
use loro::{Container, ExportMode, Frontiers, LoroDoc, LoroValue, ValueOrContainer, VersionVector};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const LORO_PACKAGE_FORMAT_VERSION: u32 = 1;
/// §A12.1.3: format version stamped on packages that CARRY a shallow
/// accelerator chunk. Old builds reject unknown chunk kinds with a
/// corruption-shaped error; the version bump turns that into the clean
/// "unsupported format version" one. Packages without a shallow chunk keep
/// v1 (fully old-build readable).
pub const LORO_PACKAGE_FORMAT_VERSION_SHALLOW: u32 = 2;

/// §A13.1.0: env-gated read-stage probe (`FLOWSTATE_OPEN_PROBE`).
fn open_probe() -> bool {
  static PROBE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
  *PROBE.get_or_init(|| std::env::var_os("FLOWSTATE_OPEN_PROBE").is_some())
}

fn package_format_version_supported(version: u32) -> bool {
  version == LORO_PACKAGE_FORMAT_VERSION || version == LORO_PACKAGE_FORMAT_VERSION_SHALLOW
}
pub const LORO_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_UPDATE_SEGMENT_COMPACTION_THRESHOLD: usize = 256;

const PACKAGE_MAGIC: &[u8; 16] = b"FLOWDB8-LORO\0\0\0\0";
const PACKAGE_HEADER_VERSION: u32 = 1;
const JOURNAL_MAGIC: &[u8; 16] = b"FLOWDB8-JOURNAL\0";
const JOURNAL_HEADER_VERSION: u32 = 1;
const JOURNAL_TXN_MAGIC: &[u8; 8] = b"DB8TXN01";
const JOURNAL_COMMIT_MAGIC: &[u8; 8] = b"DB8DONE1";
const JOURNAL_DELTA_MAGIC: &[u8; 8] = b"DB8DELTA";
const JOURNAL_GENERATION_COMPACTION_THRESHOLD: usize = 16;

const CHUNK_MANIFEST: u32 = 1;
const CHUNK_LORO_SNAPSHOT: u32 = 2;
const CHUNK_LORO_UPDATE_SEGMENT: u32 = 3;
const CHUNK_ASSET: u32 = 4;
const CHUNK_REVISION_INDEX: u32 = 5;
const CHUNK_PROJECTION_CACHE: u32 = 6;
const CHUNK_SEARCH_UNIT: u32 = 7;
const CHUNK_THUMBNAIL: u32 = 8;
const CHUNK_INTEGRITY_INDEX: u32 = 9;
const CHUNK_PREVIEW_HEADER: u32 = 10;
/// §A12.1.3 slice 1: shallow (history-truncated) Loro snapshot — an OPEN
/// ACCELERATOR chunk, never the only copy of any history (invariant S1 in
/// `flowstate_shallow_open_design.md`). Old builds reject packages carrying
/// it ("unknown Flowstate package chunk kind"), so it is only written behind
/// `FLOWSTATE_WRITE_SHALLOW_SNAPSHOT` until the format-version bump.
const CHUNK_SHALLOW_SNAPSHOT: u32 = 11;

const PACKAGE_CHUNK_TABLE_ENTRY_BYTES: usize = 4 + 8 + 8 + 32;
const MAX_PACKAGE_CHUNK_COUNT: usize = 1_048_576;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DocumentPackage {
  pub manifest: DocumentPackageManifest,
  pub loro_snapshots: Vec<LoroSnapshotChunk>,
  pub loro_update_segments: Vec<LoroUpdateSegmentChunk>,
  pub assets: Vec<AssetChunk>,
  pub revisions: Vec<PackageRevision>,
  pub projection_caches: Vec<ProjectionCacheChunk>,
  pub search_units: Vec<SearchUnitChunk>,
  pub thumbnails: Vec<ThumbnailChunk>,
  /// §19 named integrity index: one entry per durable chunk (snapshots, update
  /// segments, assets). Older packages without the index decode to an empty
  /// vector and still load; new packages always rebuild a complete index.
  #[serde(default)]
  pub integrity_index: Vec<IntegrityIndexEntry>,
  /// §act-four M2: the always-written PREVIEW HEADER — a tiny projection of the
  /// document's first `PREVIEW_HEADER_BLOCKS` blocks, written on every
  /// checkpoint even when the full projection cache is stale or absent. Preview
  /// reads ONLY this chunk (`read_preview_header`), never the snapshot, making
  /// cold preview `O(viewport)` and independent of full-cache freshness. Older
  /// packages without it decode to an empty vector.
  #[serde(default)]
  pub preview_headers: Vec<PreviewHeaderChunk>,
  /// §A12.1.3: shallow snapshot accelerator chunks (at most one is kept).
  /// Postcard is positional — this field must stay in APPEND order.
  #[serde(default)]
  pub shallow_snapshots: Vec<LoroShallowSnapshotChunk>,
  /// §A12.1.3 slice 2b: full-snapshot chunks whose BLAKE3 verification was
  /// DEFERRED at read time (open-path cost). Keyed by `snapshot_id` → the
  /// chunk-table checksum; consumers that decode those bytes verify first
  /// via [`Self::verify_snapshot_chunk`]. Never serialized.
  #[serde(skip)]
  pub(crate) deferred_snapshot_checksums: std::collections::HashMap<u128, [u8; 32]>,
}

/// §act-four M2: the maximum number of leading blocks stored in the preview
/// header — enough to fill a preview screen; independent of document size.
pub const PREVIEW_HEADER_BLOCKS: usize = 16;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DocumentPackageManifest {
  pub package_format_version: u32,
  pub loro_schema_version: u32,
  pub document_id: u128,
  pub latest_frontier: Vec<u8>,
  pub latest_version_vector: Vec<u8>,
  pub latest_snapshot_id: u128,
  pub update_segment_index: Vec<ChunkRef>,
  pub asset_index: Vec<ChunkRef>,
  pub projection_cache_frontier: Option<Vec<u8>>,
  pub search_cache_frontier: Option<Vec<u8>>,
  /// §act-four M2: frontier the preview header was written at. Older packages
  /// without it decode to `None` (preview falls back to the projection cache /
  /// full read).
  #[serde(default)]
  pub preview_header_frontier: Option<Vec<u8>>,
  /// §act-ten A10.5: RETIRED — always written empty. The M4 cold-scroll consumer
  /// was never wired, and persisting it re-ran both whole-registry scans per
  /// checkpoint. The field survives because postcard is positional: removing it
  /// would break decoding of every existing package. Old packages carrying
  /// boundaries still decode; the data is ignored.
  #[serde(default)]
  pub block_boundaries: Vec<u32>,
  pub created_at_unix_secs: i64,
  pub modified_at_unix_secs: i64,
  /// §27 schema migration log. Empty at schema v1 (no migrations exist yet).
  /// Future schema bumps append a [`SchemaMigrationRecord`] here, and any Loro
  /// document mutation a migration performs is an ordinary Loro change committed
  /// with the `"migration"` origin. Older packages without this field decode to
  /// an empty vector.
  #[serde(default)]
  pub schema_migrations: Vec<SchemaMigrationRecord>,
  /// §act-twelve A12.1.1: frontier at which the body/flow paragraph-style
  /// marks were last known CLEAN (a checkpoint from a live runtime always
  /// qualifies — the runtime maintains the marks). Open compares this to the
  /// doc frontier and SKIPS the whole-corpus style-mark verification scan on
  /// a match (measured 1.1-1.2s per open on the 2.6M-char impact doc — the
  /// single largest open cost). `None` (older packages) scans once; the next
  /// checkpoint stamps. Postcard is positional: this field must stay in
  /// APPEND order.
  #[serde(default)]
  pub style_marks_clean_frontier: Option<Vec<u8>>,
  /// §A12.1.3: frontier of the newest shallow snapshot chunk, `None` when the
  /// package carries none. The shallow chunk is an accelerator only — every
  /// consumer must fall back to the full snapshot + segments when it is
  /// absent or stale. Postcard is positional: APPEND order only.
  #[serde(default)]
  pub shallow_snapshot_frontier: Option<Vec<u8>>,
}

/// §19 integrity-index entry recording one durable chunk's identity, kind,
/// BLAKE3 checksum, and byte length so package integrity can be cross-checked
/// against the actual chunk payloads on read.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IntegrityIndexEntry {
  pub chunk_kind: u32,
  pub id: u128,
  pub checksum: [u8; 32],
  pub byte_length: u64,
}

/// §27 record of a schema migration that was applied to this package.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SchemaMigrationRecord {
  pub id: u128,
  pub from_version: u32,
  pub to_version: u32,
  pub applied_at_unix_secs: i64,
  pub description: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChunkRef {
  pub id: u128,
  pub checksum: [u8; 32],
  pub byte_length: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LoroSnapshotChunk {
  pub snapshot_id: u128,
  pub frontier: Vec<u8>,
  pub version_vector: Vec<u8>,
  #[serde(with = "serde_bytes")]
  pub bytes: Vec<u8>,
  pub created_at_unix_secs: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LoroShallowSnapshotChunk {
  pub snapshot_id: u128,
  /// The shallow ROOT: history strictly before this frontier is absent from
  /// `bytes`. A doc decoded from this chunk cannot check out, serve, or
  /// import ops below it (`flowstate_shallow_open_design.md` §1).
  pub root_frontier: Vec<u8>,
  pub root_version_vector: Vec<u8>,
  /// Frontier the snapshot was taken AT (its state tip).
  pub frontier: Vec<u8>,
  pub version_vector: Vec<u8>,
  #[serde(with = "serde_bytes")]
  pub bytes: Vec<u8>,
  pub created_at_unix_secs: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LoroUpdateSegmentChunk {
  pub segment_id: u128,
  pub from_frontier: Vec<u8>,
  pub from_version_vector: Vec<u8>,
  pub to_frontier: Vec<u8>,
  pub to_version_vector: Vec<u8>,
  #[serde(with = "serde_bytes")]
  pub bytes: Vec<u8>,
  pub checksum: [u8; 32],
  pub created_at_unix_secs: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssetChunk {
  pub asset_id: u128,
  pub content_hash: [u8; 32],
  pub mime_type: String,
  pub byte_length: u64,
  #[serde(with = "serde_bytes")]
  pub bytes: Vec<u8>,
  #[serde(with = "serde_bytes")]
  pub metadata: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PackageRevision {
  pub revision_id: u128,
  pub frontier: Vec<u8>,
  pub version_vector: Vec<u8>,
  pub title: String,
  pub summary: String,
  pub author_user_id: Option<u128>,
  pub replica_id: Option<u128>,
  pub created_at_unix_secs: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectionCacheChunk {
  pub frontier: Vec<u8>,
  #[serde(with = "serde_bytes")]
  pub bytes: Vec<u8>,
}

/// §act-four M2: a tiny first-`PREVIEW_HEADER_BLOCKS`-blocks projection for
/// `O(viewport)` cold preview. `bytes` is an encoded [`ProjectionBlocks`]
/// trimmed to the leading blocks.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreviewHeaderChunk {
  pub frontier: Vec<u8>,
  #[serde(with = "serde_bytes")]
  pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchUnitChunk {
  pub frontier: Vec<u8>,
  pub unit_id: u128,
  pub unit_kind: String,
  #[serde(default)]
  pub flow_id: Option<String>,
  #[serde(default)]
  pub block_id: Option<String>,
  #[serde(default)]
  pub table_id: Option<String>,
  #[serde(default)]
  pub cell_id: Option<String>,
  pub heading_path: Vec<String>,
  pub heading: String,
  pub body: String,
  pub insert_text: String,
  #[serde(default)]
  pub unit_start_cursor: Vec<u8>,
  #[serde(default)]
  pub unit_end_cursor: Vec<u8>,
  pub paragraph_start_cursor: Vec<u8>,
  pub paragraph_end_cursor: Vec<u8>,
}

#[derive(Clone, Debug)]
struct Chunk {
  kind: u32,
  payload: Vec<u8>,
}

#[derive(Clone, Debug)]
struct ChunkEntry {
  kind: u32,
  offset: u64,
  len: u64,
  checksum: [u8; 32],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ThumbnailChunk {
  pub thumbnail_id: u128,
  pub revision_id: Option<u128>,
  pub frontier: Vec<u8>,
  pub mime_type: String,
  #[serde(with = "serde_bytes")]
  pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
enum PackageJournalDelta {
  Update {
    manifest: DocumentPackageManifest,
    segment: LoroUpdateSegmentChunk,
    /// §act-four M2: the fresh preview header for this edit, so cold preview of
    /// an edited-but-unflushed document stays `O(viewport)` (the header rides
    /// the journal delta rather than waiting for the next full checkpoint).
    /// `None` on older journals / when no header was maintained.
    #[serde(default)]
    preview_header: Option<PreviewHeaderChunk>,
  },
  Assets {
    manifest: DocumentPackageManifest,
    assets: Vec<AssetChunk>,
  },
}

impl DocumentPackage {
  pub fn from_loro_snapshot(doc: &LoroDoc, title: &str) -> io::Result<Self> {
    Self::from_loro_snapshot_with_assets(doc, title, Vec::new())
  }

  pub fn from_loro_snapshot_with_assets(doc: &LoroDoc, title: &str, assets: Vec<AssetChunk>) -> io::Result<Self> {
    doc.commit();
    let now = unix_time_secs();
    let document_id = crate::loro_schema::document_id(doc)
      .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Loro document has no valid canonical document ID"))?
      .as_u128();
    let revision_id = Uuid::new_v4().as_u128();
    let revision_frontiers = doc.state_frontiers();
    let revision_version_vector = encode_version_vector(&frontiers_version_vector(doc, &revision_frontiers)?);
    crate::loro_schema::record_revision(doc, revision_id, encode_frontiers(&revision_frontiers), title, "Initial snapshot", None)
      .map_err(loro_io_error)?;
    let snapshot_id = Uuid::new_v4().as_u128();
    let frontier = encode_frontiers(&doc.state_frontiers());
    let version_vector = encode_version_vector(&doc.state_vv());
    let snapshot = doc.export(ExportMode::Snapshot).map_err(loro_io_error)?;
    let mut package = Self {
      manifest: DocumentPackageManifest {
        package_format_version: LORO_PACKAGE_FORMAT_VERSION,
        loro_schema_version: LORO_SCHEMA_VERSION,
        document_id,
        latest_frontier: frontier.clone(),
        latest_version_vector: version_vector.clone(),
        latest_snapshot_id: snapshot_id,
        update_segment_index: Vec::new(),
        asset_index: Vec::new(),
        projection_cache_frontier: None,
        search_cache_frontier: None,
        preview_header_frontier: None,
        block_boundaries: Vec::new(),
        created_at_unix_secs: now,
        modified_at_unix_secs: now,
        schema_migrations: Vec::new(),
        // NOT stamped here: this constructor packages ARBITRARY docs (tests,
        // converters, raw Loro states) whose marks were never maintained.
        // The RUNTIME stamps after constructing from its own maintained doc
        // (checkpoint path) and on every runtime persist (segment append).
        style_marks_clean_frontier: None,
        shallow_snapshot_frontier: None,
      },
      loro_snapshots: vec![
        LoroSnapshotChunk {
          snapshot_id,
          frontier: frontier.clone(),
          version_vector: version_vector.clone(),
          bytes: snapshot,
          created_at_unix_secs: now,
        },
        LoroSnapshotChunk {
          snapshot_id: Uuid::new_v4().as_u128(),
          frontier: encode_frontiers(&revision_frontiers),
          version_vector: revision_version_vector.clone(),
          // Full-history snapshot exported at the revision frontier directly
          // from the live doc — no O(doc) `fork_at`. We deliberately keep full
          // `SnapshotAt` bytes in the package; switching revision chunks to the
          // lighter `StateOnly` variant is an explicitly deferred decision.
          bytes: doc
            .export(ExportMode::snapshot_at(&revision_frontiers))
            .map_err(loro_io_error)?,
          created_at_unix_secs: now,
        },
      ],
      loro_update_segments: Vec::new(),
      assets,
      revisions: vec![PackageRevision {
        revision_id,
        frontier: encode_frontiers(&revision_frontiers),
        version_vector: revision_version_vector,
        title: title.to_string(),
        summary: "Initial snapshot".to_string(),
        author_user_id: None,
        replica_id: None,
        created_at_unix_secs: now,
      }],
      projection_caches: Vec::new(),
      search_units: Vec::new(),
      thumbnails: Vec::new(),
      integrity_index: Vec::new(),
      preview_headers: Vec::new(),
      shallow_snapshots: Vec::new(),
      deferred_snapshot_checksums: std::collections::HashMap::new(),
    }
    .with_manifest_indexes()?;
    package.rebuild_projection_cache_from_loro(doc)?;
    package.rebuild_search_units_from_loro(doc)?;
    fidelity::event(FidelityClass::Persistence, "save-snapshot", || {
      format!(
        "snapshots={} segments={} revisions={} assets={} frontier={}",
        package.loro_snapshots.len(),
        package.loro_update_segments.len(),
        package.revisions.len(),
        package.assets.len(),
        frontier_tag(&package.manifest.latest_frontier)
      )
    });
    package.fidelity_check_roundtrip(doc);
    Ok(package)
  }

  pub fn load_loro_doc(&self) -> io::Result<LoroDoc> {
    self.validate()?;
    self.load_loro_doc_from_validated()
  }

  /// [`Self::load_loro_doc`] minus the leading `validate()` — for callers
  /// holding a package that `read`/`from_bytes` JUST validated. The doubled
  /// validate re-hashed every update segment and asset (a second full
  /// integrity sweep over a multi-hundred-MB package) on every open.
  pub fn load_loro_doc_from_validated(&self) -> io::Result<LoroDoc> {
    let doc = self.load_loro_doc_unvalidated()?;
    fidelity::event(FidelityClass::Persistence, "load", || {
      format!(
        "snapshots={} segments={} revisions={} frontier={}",
        self.loro_snapshots.len(),
        self.loro_update_segments.len(),
        self.revisions.len(),
        frontier_tag(&self.manifest.latest_frontier)
      )
    });
    Ok(doc)
  }

  /// §fidelity persistence round-trip invariant. Read-only and gated on
  /// [`fidelity::expensive_checks_enabled`]: it reloads this package's canonical Loro state into a
  /// fresh document and asserts the projection built from the reloaded state
  /// matches the projection of the `source` document that was persisted
  /// (paragraph count + texts, paragraph/block ids, and section count). Never
  /// mutates `self` or `source`; failures fire the `package-roundtrip-mismatch`
  /// violation.
  fn fidelity_check_roundtrip(&self, source: &LoroDoc) {
    if !fidelity::expensive_checks_enabled() {
      return;
    }
    let expected = match crate::loro_projection::document_from_loro(source) {
      Ok(document) => document,
      Err(error) => {
        fidelity::violation(FidelityClass::Persistence, "package-roundtrip-mismatch", || {
          format!("could not project the source document for the round-trip check: {error}")
        });
        return;
      },
    };
    let loaded = match self
      .load_loro_doc_unvalidated()
      .and_then(|doc| crate::loro_projection::document_from_loro(&doc))
    {
      Ok(document) => document,
      Err(error) => {
        fidelity::violation(FidelityClass::Persistence, "package-roundtrip-mismatch", || {
          format!("could not reload and project the persisted package: {error}")
        });
        return;
      },
    };
    let same_shape = expected.paragraphs.len() == loaded.paragraphs.len()
      && expected.ids.paragraph_ids == loaded.ids.paragraph_ids
      && expected.ids.block_ids == loaded.ids.block_ids
      && expected.sections.len() == loaded.sections.len();
    let same_text =
      same_shape && (0..expected.paragraphs.len()).all(|ix| crate::paragraph_text(&expected, ix) == crate::paragraph_text(&loaded, ix));
    fidelity::check(same_shape && same_text, FidelityClass::Persistence, "package-roundtrip-mismatch", || {
      format!(
        "reloaded projection differs from persisted source: paragraphs {}->{}, sections {}->{}, ids_match={}, text_match={}",
        expected.paragraphs.len(),
        loaded.paragraphs.len(),
        expected.sections.len(),
        loaded.sections.len(),
        expected.ids.paragraph_ids == loaded.ids.paragraph_ids && expected.ids.block_ids == loaded.ids.block_ids,
        same_text
      )
    });
  }

  /// §A12.1.3 slice 2b: verify a snapshot chunk whose hash was deferred at
  /// read time. No-op for chunks verified eagerly (or built in memory). The
  /// deferred value is the chunk-TABLE checksum — a hash of the postcard
  /// envelope — so verification re-encodes the chunk (a memcpy-scale pass;
  /// postcard is deterministic) and hashes that.
  fn verify_snapshot_chunk(&self, snapshot: &LoroSnapshotChunk) -> io::Result<()> {
    if let Some(expected) = self.deferred_snapshot_checksums.get(&snapshot.snapshot_id) {
      let payload = encode_chunk(snapshot, "Loro snapshot")?;
      if blake3_hash(&payload) != *expected {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "Flowstate package chunk checksum mismatch"));
      }
    }
    Ok(())
  }

  /// §A12.1.3 slice 2b: integrity-index checksum to VALIDATE against for a
  /// snapshot chunk. For read-deferred chunks the stored entry is echoed
  /// (its real verification happens at decode via
  /// [`Self::verify_snapshot_chunk`]); everything else hashes for real.
  fn integrity_checksum_for_snapshot(&self, snapshot: &LoroSnapshotChunk) -> [u8; 32] {
    if self
      .deferred_snapshot_checksums
      .contains_key(&snapshot.snapshot_id)
      && let Some(entry) = self
        .integrity_index
        .iter()
        .find(|entry| entry.chunk_kind == CHUNK_LORO_SNAPSHOT && entry.id == snapshot.snapshot_id)
    {
      return entry.checksum;
    }
    blake3_hash(&snapshot.bytes)
  }

  fn load_loro_doc_unvalidated(&self) -> io::Result<LoroDoc> {
    let snapshot = self
      .latest_snapshot()
      .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Flowstate package has no latest Loro snapshot"))?;
    self.verify_snapshot_chunk(snapshot)?;
    let doc = LoroDoc::new();
    crate::loro_schema::configure_text_styles(&doc);
    doc.import(&snapshot.bytes).map_err(loro_io_error)?;
    for segment in &self.loro_update_segments {
      doc.import(&segment.bytes).map_err(loro_io_error)?;
    }
    let document_id = crate::loro_schema::document_id(&doc)
      .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "package Loro state has no valid document ID"))?;
    if document_id.as_u128() != self.manifest.document_id {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "package manifest document ID does not match canonical Loro lineage",
      ));
    }
    if crate::loro_schema::document_schema_version(&doc) != Some(self.manifest.loro_schema_version) {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "package manifest schema version does not match canonical Loro metadata",
      ));
    }
    Ok(doc)
  }
  /// §A12.1.3 slice 2: load the live doc from the SHALLOW accelerator chunk
  /// plus the update segments, skipping the full-history decode. Returns
  /// `Ok(None)` when the package has no usable shallow chunk; the caller
  /// falls back to [`Self::load_loro_doc_from_validated`]. Any import/lineage
  /// anomaly is an `Err` the caller likewise treats as fall-back-to-full
  /// (design: `flowstate_shallow_open_design.md` §4).
  ///
  /// Segment overlap is safe by construction: a shallow doc's oplog VV covers
  /// genesis→tip (only its HISTORY is truncated), so Loro's update decode
  /// drops already-known ops; segments at-or-below the shallow frontier
  /// decode to nothing. A segment op with deps below the shallow ROOT (a
  /// pre-shallow far-behind remote import persisted before this open) errors
  /// cleanly (`ImportUpdatesThatDependsOnOutdatedVersion`) → full fallback.
  pub fn load_loro_doc_shallow(&self) -> io::Result<Option<LoroDoc>> {
    let Some(stamp) = self.manifest.shallow_snapshot_frontier.as_deref() else {
      return Ok(None);
    };
    let Some(shallow) = self
      .shallow_snapshots
      .iter()
      .rev()
      .find(|shallow| shallow.frontier == stamp)
    else {
      return Ok(None);
    };
    let doc = LoroDoc::new();
    crate::loro_schema::configure_text_styles(&doc);
    doc.import(&shallow.bytes).map_err(loro_io_error)?;
    for segment in &self.loro_update_segments {
      doc.import(&segment.bytes).map_err(loro_io_error)?;
    }
    // The reconstructed tip must be exactly the manifest tip — anything else
    // (a stale chunk, a segment the shallow state couldn't accept) demotes to
    // the full path rather than opening a wrong version.
    if encode_frontiers(&doc.oplog_frontiers()) != self.manifest.latest_frontier {
      return Ok(None);
    }
    let document_id = crate::loro_schema::document_id(&doc)
      .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "package Loro state has no valid document ID"))?;
    if document_id.as_u128() != self.manifest.document_id {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "package manifest document ID does not match canonical Loro lineage",
      ));
    }
    if crate::loro_schema::document_schema_version(&doc) != Some(self.manifest.loro_schema_version) {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "package manifest schema version does not match canonical Loro metadata",
      ));
    }
    Ok(Some(doc))
  }

  pub fn replace_assets_from_document(&mut self, document: &crate::DocumentProjection) -> io::Result<()> {
    let mut next = self.clone();
    next.assets = crate::loro_import::assets_from_document(document);
    next.manifest.modified_at_unix_secs = unix_time_secs();
    next.refresh_manifest_indexes();
    next.validate()?;
    *self = next;
    Ok(())
  }

  pub fn current_search_units(&self) -> &[SearchUnitChunk] {
    if self.manifest.search_cache_frontier.as_deref() == Some(self.manifest.latest_frontier.as_slice()) {
      &self.search_units
    } else {
      &[]
    }
  }

  pub fn read_cached_search_units(path: impl AsRef<Path>) -> io::Result<Option<Vec<SearchUnitChunk>>> {
    let bytes = fs::read(path)?;
    Self::cached_search_units_from_bytes(&bytes)
  }

  pub fn cached_search_units_from_bytes(bytes: &[u8]) -> io::Result<Option<Vec<SearchUnitChunk>>> {
    let (manifest, units) = if bytes.starts_with(JOURNAL_MAGIC) {
      cached_search_units_from_journal_bytes(bytes)?
    } else {
      cached_search_units_from_compact_bytes(bytes)?
    };
    if !package_format_version_supported(manifest.package_format_version) || manifest.loro_schema_version != LORO_SCHEMA_VERSION {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "unsupported Flowstate cached search package version",
      ));
    }
    let Some(search_frontier) = manifest.search_cache_frontier.as_deref() else {
      return Ok(None);
    };
    validate_frontiers(search_frontier, "manifest search cache frontier")?;
    validate_frontiers(&manifest.latest_frontier, "manifest latest frontier")?;
    if search_frontier != manifest.latest_frontier.as_slice() {
      return Ok(None);
    }
    if units.iter().any(|unit| unit.frontier != search_frontier) {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "search unit frontier does not match package cache frontier",
      ));
    }
    Ok(Some(units))
  }

  pub fn append_update_segment(
    &mut self,
    from_frontier: &Frontiers,
    from_version_vector: &VersionVector,
    to_frontier: &Frontiers,
    to_version_vector: &VersionVector,
    bytes: Vec<u8>,
  ) -> io::Result<u128> {
    if bytes.is_empty() {
      return Err(io::Error::new(io::ErrorKind::InvalidInput, "cannot append an empty Loro update segment"));
    }
    let segment_id = Uuid::new_v4().as_u128();
    let now = unix_time_secs();
    let checksum = blake3_hash(&bytes);
    // Append IN PLACE with a rollback guard instead of `self.clone()`: the
    // clone copied every snapshot/asset chunk — ~86MB on a large document —
    // on EVERY keystroke inside the write gate (the .db8 first-edit freeze).
    // Only the manifest and the cleared caches need restoring on a validation
    // failure; the pushed segment pops back off.
    let manifest_backup = self.manifest.clone();
    let projection_caches_backup = std::mem::take(&mut self.projection_caches);
    let search_units_backup = std::mem::take(&mut self.search_units);
    self.loro_update_segments.push(LoroUpdateSegmentChunk {
      segment_id,
      from_frontier: encode_frontiers(from_frontier),
      from_version_vector: encode_version_vector(from_version_vector),
      to_frontier: encode_frontiers(to_frontier),
      to_version_vector: encode_version_vector(to_version_vector),
      bytes,
      checksum,
      created_at_unix_secs: now,
    });
    self.manifest.latest_frontier = encode_frontiers(to_frontier);
    self.manifest.latest_version_vector = encode_version_vector(to_version_vector);
    self.manifest.projection_cache_frontier = None;
    self.manifest.search_cache_frontier = None;
    // §act-twelve A12.1.1: only live runtimes write packages, and the runtime
    // maintains paragraph-style marks through every local/import/repair flow —
    // the appended segment's frontier is therefore marks-clean, and open can
    // skip the whole-corpus verification scan.
    self.manifest.style_marks_clean_frontier = Some(encode_frontiers(to_frontier));
    self.manifest.modified_at_unix_secs = now;
    self.refresh_manifest_indexes();
    // Validate ONLY the delta: full `validate()` re-hashes every stored
    // segment and decodes every cached projection — O(package) per keystroke
    // (the other half of the .db8 first-edit freeze). Full validation still
    // runs at read/write time.
    if let Err(error) = self.validate_appended_segment() {
      self.loro_update_segments.pop();
      self.manifest = manifest_backup;
      self.projection_caches = projection_caches_backup;
      self.search_units = search_units_backup;
      self.refresh_manifest_indexes();
      return Err(error);
    }
    fidelity::event(FidelityClass::Persistence, "append-segment", || {
      format!(
        "segment={segment_id:032x} segments={} frontier={}",
        self.loro_update_segments.len(),
        frontier_tag(&self.manifest.latest_frontier)
      )
    });
    Ok(segment_id)
  }

  pub fn create_named_revision(
    &mut self,
    doc: &LoroDoc,
    title: impl Into<String>,
    summary: impl Into<String>,
    author_user_id: Option<u128>,
    replica_id: Option<u128>,
  ) -> io::Result<u128> {
    self.create_named_revision_with_id(doc, Uuid::new_v4().as_u128(), title, summary, author_user_id, replica_id)
  }

  pub fn create_named_revision_with_id(
    &mut self,
    doc: &LoroDoc,
    revision_id: u128,
    title: impl Into<String>,
    summary: impl Into<String>,
    author_user_id: Option<u128>,
    replica_id: Option<u128>,
  ) -> io::Result<u128> {
    self.create_named_revision_at_with_id(doc, revision_id, &doc.state_frontiers(), title, summary, author_user_id, replica_id)
  }

  pub fn create_named_revision_at_with_id(
    &mut self,
    doc: &LoroDoc,
    revision_id: u128,
    frontiers: &Frontiers,
    title: impl Into<String>,
    summary: impl Into<String>,
    author_user_id: Option<u128>,
    replica_id: Option<u128>,
  ) -> io::Result<u128> {
    doc.commit();
    let title = title.into();
    let summary = summary.into();
    if self
      .revisions
      .iter()
      .any(|revision| revision.revision_id == revision_id)
    {
      return Ok(revision_id);
    }
    let doc_frontier_before_revision_record = doc.state_frontiers();
    let doc_vv_before_revision_record = doc.state_vv();
    let frontier = encode_frontiers(frontiers);
    let version_vector = encode_version_vector(&frontiers_version_vector(doc, frontiers)?);
    if !loro_revision_exists(doc, revision_id) {
      crate::loro_schema::record_revision(doc, revision_id, frontier.clone(), &title, &summary, author_user_id).map_err(loro_io_error)?;
      let update = doc
        .export(ExportMode::updates(&doc_vv_before_revision_record))
        .map_err(loro_io_error)?;
      if !update.is_empty() {
        self.append_update_segment(
          &doc_frontier_before_revision_record,
          &doc_vv_before_revision_record,
          &doc.state_frontiers(),
          &doc.state_vv(),
          update,
        )?;
      }
    }
    let revision = PackageRevision {
      revision_id,
      frontier: frontier.clone(),
      version_vector,
      title,
      summary,
      author_user_id,
      replica_id,
      created_at_unix_secs: unix_time_secs(),
    };
    let revision_id = revision.revision_id;
    // §A12.1.3: `SnapshotAt` export is upstream-unimplemented on shallow
    // docs. The stored chunk is only a revision-LOAD accelerator — without
    // it, `load_revision_loro_doc` falls back to forking the package's full
    // doc at the revision frontier (S1 keeps that history), so a shallow
    // session simply skips the accelerator.
    if self.snapshot_for_frontier(&frontier).is_none() && !doc.is_shallow() {
      self.loro_snapshots.push(LoroSnapshotChunk {
        snapshot_id: Uuid::new_v4().as_u128(),
        frontier: frontier.clone(),
        version_vector: revision.version_vector.clone(),
        // Full `SnapshotAt` bytes from the live doc (no fork); the lighter
        // `StateOnly` variant is an explicitly deferred format decision.
        bytes: doc
          .export(ExportMode::snapshot_at(frontiers))
          .map_err(loro_io_error)?,
        created_at_unix_secs: unix_time_secs(),
      });
    }
    self.revisions.push(revision);
    self.manifest.modified_at_unix_secs = unix_time_secs();
    // §19: a revision may add a snapshot chunk; keep the integrity index complete.
    self.integrity_index = self.build_integrity_index();
    self.validate()?;
    fidelity::event(FidelityClass::Persistence, "checkpoint", || {
      format!(
        "revision={revision_id:032x} revisions={} snapshots={} frontier={}",
        self.revisions.len(),
        self.loro_snapshots.len(),
        frontier_tag(&frontier)
      )
    });
    Ok(revision_id)
  }

  pub fn load_revision_loro_doc(&self, revision_id: u128) -> io::Result<LoroDoc> {
    let revision = self
      .revisions
      .iter()
      .find(|revision| revision.revision_id == revision_id)
      .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Flowstate package revision is missing"))?;
    fidelity::event(FidelityClass::Persistence, "load-revision", || {
      format!("revision={:032x} frontier={}", revision.revision_id, frontier_tag(&revision.frontier))
    });
    if let Some(snapshot) = self.snapshot_for_frontier(&revision.frontier) {
      self.verify_snapshot_chunk(snapshot)?;
      let doc = LoroDoc::new();
      crate::loro_schema::configure_text_styles(&doc);
      doc.import(&snapshot.bytes).map_err(loro_io_error)?;
      return Ok(doc);
    }
    let doc = self.load_loro_doc()?;
    let frontiers = decode_frontiers(&revision.frontier)?;
    doc.fork_at(&frontiers).map_err(loro_io_error)
  }

  pub fn compact_to_named_snapshot(
    &mut self,
    doc: &LoroDoc,
    title: impl Into<String>,
    summary: impl Into<String>,
    author_user_id: Option<u128>,
    replica_id: Option<u128>,
  ) -> io::Result<(u128, u128)> {
    self.compact_to_named_snapshot_with_id(doc, Uuid::new_v4().as_u128(), title, summary, author_user_id, replica_id)
  }

  pub fn sync_revisions_from_loro(&mut self, doc: &LoroDoc) -> io::Result<usize> {
    let root = doc.get_map(crate::loro_schema::ROOT);
    let Some(ValueOrContainer::Container(Container::List(revisions))) = root.get(crate::loro_schema::REVISIONS) else {
      return Ok(0);
    };
    let mut added = 0usize;
    for index in 0..revisions.len() {
      let Some(ValueOrContainer::Container(Container::Map(revision))) = revisions.get(index) else {
        continue;
      };
      let Some(revision_id) = package_map_string(&revision, "id").and_then(|id| id.parse::<u128>().ok()) else {
        continue;
      };
      if self
        .revisions
        .iter()
        .any(|existing| existing.revision_id == revision_id)
      {
        continue;
      }
      let Some(frontier) = package_map_binary(&revision, "frontier") else {
        continue;
      };
      let frontiers = decode_frontiers(&frontier)?;
      let version_vector = encode_version_vector(&frontiers_version_vector(doc, &frontiers)?);
      if self.snapshot_for_frontier(&frontier).is_none() {
        self.loro_snapshots.push(LoroSnapshotChunk {
          snapshot_id: Uuid::new_v4().as_u128(),
          frontier: frontier.clone(),
          version_vector: version_vector.clone(),
          // This runs on every remote import that learns a new revision, so it
          // must stay O(revision) — export full `SnapshotAt` bytes straight
          // from the live doc instead of paying an O(doc) `fork_at`. The
          // lighter `StateOnly` variant is an explicitly deferred decision.
          bytes: doc
            .export(ExportMode::snapshot_at(&frontiers))
            .map_err(loro_io_error)?,
          created_at_unix_secs: package_map_i64(&revision, "timestamp").unwrap_or_else(unix_time_secs),
        });
      }
      self.revisions.push(PackageRevision {
        revision_id,
        frontier,
        version_vector,
        title: package_map_string(&revision, "title").unwrap_or_else(|| "Revision".to_string()),
        summary: package_map_string(&revision, "summary").unwrap_or_default(),
        author_user_id: package_map_string(&revision, "author_user_id").and_then(|id| id.parse().ok()),
        replica_id: package_map_string(&revision, "replica_id").and_then(|id| id.parse().ok()),
        created_at_unix_secs: package_map_i64(&revision, "timestamp").unwrap_or_else(unix_time_secs),
      });
      added += 1;
    }
    if added > 0 {
      self
        .revisions
        .sort_by_key(|revision| revision.created_at_unix_secs);
      self.manifest.modified_at_unix_secs = unix_time_secs();
      // §19: syncing revisions may add snapshot chunks; keep the index complete.
      self.integrity_index = self.build_integrity_index();
      self.validate()?;
    }
    Ok(added)
  }

  pub fn compact_to_named_snapshot_with_id(
    &mut self,
    doc: &LoroDoc,
    revision_id: u128,
    title: impl Into<String>,
    summary: impl Into<String>,
    author_user_id: Option<u128>,
    replica_id: Option<u128>,
  ) -> io::Result<(u128, u128)> {
    let snapshot_id = self.compact_to_snapshot_any(doc)?;
    let revision_id = self.create_named_revision_with_id(doc, revision_id, title, summary, author_user_id, replica_id)?;
    Ok((revision_id, snapshot_id))
  }

  /// §A12.1.3 slice 1 (dark chunk): export a shallow snapshot of `doc`
  /// rooted at (a causal floor of) `requested_root` and store it as the
  /// package's SINGLE shallow accelerator chunk, stamping
  /// `manifest.shallow_snapshot_frontier`. The full snapshot and update
  /// segments are untouched — invariant S1 (`flowstate_shallow_open_design
  /// .md` §2): the shallow chunk is never the only copy of any history.
  ///
  /// Loro floors the requested root to a causally-closed cut, so the TRUE
  /// root is read back from a scratch decode of the exported bytes (bounded
  /// by O(state + window); this runs on checkpoint paths, never per edit).
  pub fn record_shallow_snapshot(&mut self, doc: &LoroDoc, requested_root: &Frontiers) -> io::Result<u128> {
    doc.commit();
    let bytes = doc
      .export(ExportMode::shallow_snapshot(requested_root))
      .map_err(loro_io_error)?;
    // §A13.4.3: the true (floored) root used to be read back from a scratch
    // decode of the exported bytes (~60ms per checkpoint). For a SINGLE-HEAD
    // requested root the floor is the identity (`calc_shallow_doc_start`
    // only reduces multi-head frontiers), and the export's start-vv law is
    // `frontiers_to_vv(root)` with each head clamped DOWN to its own counter
    // (the root op itself rides the snapshot). Multi-head roots — and
    // `FLOWSTATE_SHALLOW_ROOT_VERIFY`, which decodes and asserts equality —
    // take the probe path.
    static ROOT_VERIFY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let verify = *ROOT_VERIFY.get_or_init(|| std::env::var_os("FLOWSTATE_SHALLOW_ROOT_VERIFY").is_some());
    let analytic = if requested_root.len() <= 1 {
      let mut root_version_vector = doc
        .frontiers_to_vv(requested_root)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "shallow root frontier is not in the document history"))?;
      for id in requested_root.iter() {
        root_version_vector.insert(id.peer, id.counter);
      }
      Some((requested_root.clone(), root_version_vector, doc.oplog_frontiers(), doc.oplog_vv()))
    } else {
      None
    };
    let (root_frontier, root_version_vector, at_frontiers, at_vv) = match analytic {
      Some(analytic) if !verify => analytic,
      analytic => {
        let probe = LoroDoc::new();
        probe.import(&bytes).map_err(loro_io_error)?;
        let decoded = (
          probe.shallow_since_frontiers(),
          probe.shallow_since_vv().to_vv(),
          probe.oplog_frontiers(),
          probe.oplog_vv(),
        );
        if verify && let Some(analytic) = analytic {
          assert!(
            analytic.0 == decoded.0 && analytic.1 == decoded.1 && analytic.2 == decoded.2 && analytic.3 == decoded.3,
            "[flowstate-shallow-root-verify] analytic shallow root diverges from the decoded chunk"
          );
        }
        decoded
      },
    };
    let snapshot_id = Uuid::new_v4().as_u128();
    self.shallow_snapshots.clear();
    let frontier = encode_frontiers(&at_frontiers);
    self.shallow_snapshots.push(LoroShallowSnapshotChunk {
      snapshot_id,
      root_frontier: encode_frontiers(&root_frontier),
      root_version_vector: encode_version_vector(&root_version_vector),
      frontier: frontier.clone(),
      version_vector: encode_version_vector(&at_vv),
      bytes,
      created_at_unix_secs: unix_time_secs(),
    });
    self.manifest.shallow_snapshot_frontier = Some(frontier);
    self.manifest.package_format_version = LORO_PACKAGE_FORMAT_VERSION_SHALLOW;
    self.refresh_manifest_indexes();
    Ok(snapshot_id)
  }

  /// §A12.4.1: refresh the shallow accelerator at the live tip WITHOUT
  /// compacting (root policy: previous checkpoint frontier, as in
  /// [`Self::compact_to_snapshot`]'s mint). Non-fatal by design — the chunk
  /// is an accelerator (S1); on failure the previous chunk is dropped
  /// rather than left stale-stamped.
  pub fn refresh_shallow_snapshot(&mut self, live: &LoroDoc) {
    // §A13.4.3: the shallow export costs ~200ms on a large doc — do NOT pay
    // it per routine checkpoint. A slightly stale accelerator only costs the
    // next open a replay of the segments past it (Loro drops known ops at
    // decode), so refresh only once enough segments have accumulated beyond
    // the chunk. Consolidations (`compact_to_snapshot_any`) always re-mint.
    const MAX_SEGMENTS_PAST_SHALLOW: usize = 64;
    if let Some(shallow) = self.latest_shallow_snapshot() {
      let past = match self
        .loro_update_segments
        .iter()
        .position(|segment| segment.from_frontier == shallow.frontier)
      {
        Some(position) => self.loro_update_segments.len() - position,
        // Chunk frontier not on the segment chain: at the base snapshot
        // frontier (all segments are past it) or off-chain (stale) — count
        // everything either way.
        None => self.loro_update_segments.len(),
      };
      if past < MAX_SEGMENTS_PAST_SHALLOW {
        return;
      }
    }
    let previous_checkpoint_frontier = self.manifest.latest_frontier.clone();
    let requested_root = decode_frontiers(&previous_checkpoint_frontier).unwrap_or_default();
    if let Err(error) = self.record_shallow_snapshot(live, &requested_root) {
      eprintln!("[flowstate-shallow-open] refreshing the shallow snapshot failed (package keeps the full path): {error}");
      self.clear_shallow_snapshot();
    }
  }

  /// §A12.1.3: the package's shallow accelerator chunk, if present.
  pub fn latest_shallow_snapshot(&self) -> Option<&LoroShallowSnapshotChunk> {
    self.shallow_snapshots.last()
  }

  /// §A12.1.3: drop the shallow accelerator chunk (e.g. when it goes stale
  /// relative to a rewritten history). Always safe — it is never the only
  /// copy of anything (invariant S1).
  pub fn clear_shallow_snapshot(&mut self) {
    self.shallow_snapshots.clear();
    self.manifest.shallow_snapshot_frontier = None;
    self.manifest.package_format_version = LORO_PACKAGE_FORMAT_VERSION;
    self.refresh_manifest_indexes();
  }

  /// §A12.1.3 slice 3: reconstruct the FULL-history doc from this package's
  /// full snapshot chunk + update segments, caught up to `tip`'s frontier via
  /// a post-root updates import (always legal: every op past the package's
  /// persisted state is post-root by construction). This is the
  /// load-serve-drop sidecar — transient, never held by the editor.
  pub fn reconstruct_full_doc(&self, tip: &LoroDoc) -> io::Result<LoroDoc> {
    let full = self.load_loro_doc_from_validated()?;
    if full.is_shallow() {
      // The package's "full" chunk is itself shallow — S1 was violated by an
      // earlier writer. Fail loudly; compaction from this doc would compound
      // the history loss.
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "package full-history snapshot is shallow (S1 violation); refusing sidecar reconstruction",
      ));
    }
    let missing = tip
      .export(ExportMode::updates(&full.oplog_vv()))
      .map_err(loro_io_error)?;
    if !missing.is_empty() {
      full.import(&missing).map_err(loro_io_error)?;
    }
    if full.oplog_frontiers() != tip.oplog_frontiers() {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "sidecar reconstruction did not reach the live tip",
      ));
    }
    Ok(full)
  }

  /// §A12.1.3 slice 3: compaction that is correct for BOTH full and shallow
  /// live docs. A shallow live doc cannot full-export (its snapshot export is
  /// silently shallow — see the guard in [`Self::compact_to_snapshot`]), so
  /// the full-history doc is reconstructed from the package (sidecar) and
  /// compaction runs from IT; the shallow accelerator is then re-minted at
  /// the new checkpoint so the next open stays fast.
  pub fn compact_to_snapshot_any(&mut self, live: &LoroDoc) -> io::Result<u128> {
    if !live.is_shallow() {
      return self.compact_to_snapshot(live);
    }
    let previous_checkpoint_frontier = self.manifest.latest_frontier.clone();
    let full = self.reconstruct_full_doc(live)?;
    let snapshot_id = self.compact_to_snapshot(&full)?;
    let requested_root = decode_frontiers(&previous_checkpoint_frontier).unwrap_or_default();
    self.record_shallow_snapshot(live, &requested_root)?;
    Ok(snapshot_id)
  }

  pub fn compact_to_snapshot(&mut self, doc: &LoroDoc) -> io::Result<u128> {
    // §A12.1.3 invariant S1: a shallow doc's `Snapshot` export is silently
    // SHALLOW — storing it as the full chunk and clearing segments would
    // orphan all pre-root history. Consolidation of a shallow live doc must
    // come from the full-history sidecar (design §3); until that lands,
    // refuse loudly rather than lose history.
    if doc.is_shallow() {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "refusing to compact from a shallow live doc: its snapshot export would orphan pre-root history",
      ));
    }
    doc.commit();
    // §A12.1.3 §7 root policy: the shallow root trails by one checkpoint —
    // the manifest frontier BEFORE this compaction.
    let previous_checkpoint_frontier = self.manifest.latest_frontier.clone();
    let snapshot_id = Uuid::new_v4().as_u128();
    let now = unix_time_secs();
    let frontier = encode_frontiers(&doc.state_frontiers());
    let version_vector = encode_version_vector(&doc.state_vv());
    let bytes = doc.export(ExportMode::Snapshot).map_err(loro_io_error)?;
    let mut next = self.clone();
    next.loro_snapshots.push(LoroSnapshotChunk {
      snapshot_id,
      frontier: frontier.clone(),
      version_vector,
      bytes,
      created_at_unix_secs: now,
    });
    next.loro_update_segments.clear();
    next.manifest.latest_snapshot_id = snapshot_id;
    next.manifest.style_marks_clean_frontier = Some(frontier.clone());
    next.manifest.latest_frontier = frontier;
    next.manifest.latest_version_vector = encode_version_vector(&doc.state_vv());
    next.manifest.modified_at_unix_secs = now;
    let retained_revision_frontiers = next
      .revisions
      .iter()
      .map(|revision| revision.frontier.clone())
      .collect::<Vec<_>>();
    next.loro_snapshots.retain(|snapshot| {
      snapshot.snapshot_id == snapshot_id
        || retained_revision_frontiers
          .iter()
          .any(|frontier| frontier.as_slice() == snapshot.frontier.as_slice())
    });
    next.refresh_manifest_indexes();
    next.rebuild_projection_cache_from_loro(doc)?;
    next.rebuild_search_units_from_loro(doc)?;
    // §A12.1.3 (default ON): also mint the shallow accelerator chunk so the
    // next open takes the shallow path. Failures are non-fatal — the chunk
    // is an accelerator, never a required artifact (invariant S1).
    static WRITE_SHALLOW: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *WRITE_SHALLOW.get_or_init(|| std::env::var_os("FLOWSTATE_DISABLE_SHALLOW_SNAPSHOT").is_none()) {
      let requested_root = decode_frontiers(&previous_checkpoint_frontier).unwrap_or_default();
      if let Err(error) = next.record_shallow_snapshot(doc, &requested_root) {
        eprintln!("[flowstate-shallow-open] minting the shallow snapshot failed (package keeps the full path): {error}");
        next.clear_shallow_snapshot();
      }
    }
    *self = next;
    // §fidelity persistence: record the compaction and assert that compaction did
    // not drop the ability to reload any named revision, and that the compacted
    // package still round-trips to the source projection. The revision reload
    // loop is eager, so gate the whole block on `enabled()`.
    if fidelity::enabled() {
      fidelity::event(FidelityClass::Persistence, "compact", || {
        format!(
          "snapshot={snapshot_id:032x} snapshots={} segments={} revisions={} frontier={}",
          self.loro_snapshots.len(),
          self.loro_update_segments.len(),
          self.revisions.len(),
          frontier_tag(&self.manifest.latest_frontier)
        )
      });
      for revision in &self.revisions {
        let loadable = self.load_revision_loro_doc(revision.revision_id).is_ok();
        fidelity::check(loadable, FidelityClass::Persistence, "revision-unloadable-after-compact", || {
          format!("named revision {:032x} is not loadable after compaction", revision.revision_id)
        });
      }
      self.fidelity_check_roundtrip(doc);
    }
    Ok(snapshot_id)
  }

  pub fn compact_update_segments_if_needed(&mut self, doc: &LoroDoc, max_update_segments: usize) -> io::Result<Option<u128>> {
    if max_update_segments == 0 || self.loro_update_segments.len() <= max_update_segments {
      return Ok(None);
    }

    self.compact_to_snapshot_any(doc).map(Some)
  }

  pub fn rebuild_search_units_from_loro(&mut self, doc: &LoroDoc) -> io::Result<()> {
    doc.commit();
    let frontier = encode_frontiers(&doc.state_frontiers());
    self.search_units = crate::package_search::search_units_from_loro(doc, self.manifest.document_id, &frontier)?;
    self.manifest.search_cache_frontier = Some(frontier);
    self.manifest.modified_at_unix_secs = unix_time_secs();
    self.validate()?;
    Ok(())
  }

  pub fn read(path: impl AsRef<Path>) -> io::Result<Self> {
    let path = path.as_ref();
    let probe_t0 = std::time::Instant::now();
    let bytes = fs::read(path)?;
    if open_probe() {
      eprintln!("[flowstate-read-probe] fs_read={:?} bytes={}", probe_t0.elapsed(), bytes.len());
    }
    let package = Self::from_bytes(&bytes)?;
    if bytes.starts_with(JOURNAL_MAGIC) {
      let (_, committed_end) = committed_journal_transactions(&bytes)?;
      if committed_end != bytes.len() {
        let file = OpenOptions::new().write(true).open(path)?;
        file.set_len(committed_end as u64)?;
        file.sync_all()?;
      }
    }
    Ok(package)
  }

  /// Preview fast path (§P3): materialize a [`crate::DocumentProjection`]
  /// from a package file using ONLY its manifest, frontier-current projection
  /// cache, and asset chunks. Snapshot/segment chunks are neither hashed,
  /// copied, nor decoded, and no `validate()` sweep runs — on a large
  /// package the full read spent seconds in exactly those (the ".db8 preview
  /// slower than the equivalent .docx" field report). Returns `Ok(None)`
  /// when no frontier-current cache exists; callers fall back to the full
  /// read + Loro materialization. Read-only: never truncates a journal tail.
  pub fn read_cached_projection(path: impl AsRef<Path>) -> io::Result<Option<crate::DocumentProjection>> {
    let bytes = fs::read(path)?;
    let cached = if bytes.starts_with(JOURNAL_MAGIC) {
      cached_projection_from_journal_bytes(&bytes)?
    } else {
      cached_projection_from_compact_bytes(&bytes)?
    };
    let Some((manifest, caches, assets)) = cached else {
      return Ok(None);
    };
    let Some(frontier) = manifest.projection_cache_frontier.as_deref() else {
      return Ok(None);
    };
    if frontier != manifest.latest_frontier.as_slice() {
      return Ok(None);
    }
    let Some(cache) = caches.iter().find(|cache| cache.frontier == frontier) else {
      return Ok(None);
    };
    let projection = decode_chunk::<crate::loro_projection::ProjectionBlocks>(&cache.bytes, "projection cache payload")?;
    let mut document = crate::loro_projection::document_from_projection_blocks(projection);
    if document.ids.document_id == 0 {
      document.ids.document_id = manifest.document_id;
    }
    document.frontier = frontier.to_vec();
    crate::attach_package_assets(&mut document, &assets);
    Ok(Some(document))
  }

  /// §act-four M2: the CHEAPEST cold preview — decode ONLY the manifest + the
  /// frontier-current preview header (the leading `PREVIEW_HEADER_BLOCKS`
  /// blocks), never the snapshot, segments, projection cache, or assets. This
  /// is `O(viewport)` regardless of document size, and — because the header
  /// rides journal deltas — stays fresh for edited-but-unflushed documents
  /// where the full projection cache is absent. Returns `Ok(None)` when no
  /// frontier-current header exists (the caller falls back to
  /// `read_cached_projection` → full read). Read-only.
  pub fn read_preview_header(path: impl AsRef<Path>) -> io::Result<Option<crate::DocumentProjection>> {
    // §act-ten A10.13: POSITIONED reads. The old `fs::read` pulled the whole
    // package (377 MB for a large .db8) to decode a 16-block header — the M2
    // "O(viewport)" claim was false on the I/O axis. Now: seek-walk the journal
    // transaction headers (tiny reads), pread only the last full generation's
    // chunk TABLE plus its manifest/preview chunks, and read the (small) delta
    // payloads after it. Falls back to the whole-file path on any read error
    // so a torn/foreign file degrades exactly as before.
    let extracted = match preview_header_positioned(path.as_ref()) {
      Ok(extracted) => extracted,
      Err(_) => {
        let bytes = fs::read(path)?;
        if bytes.starts_with(JOURNAL_MAGIC) {
          preview_header_from_journal_bytes(&bytes)?
        } else {
          preview_header_from_compact_bytes(&bytes)?
        }
      },
    };
    let Some((manifest, headers)) = extracted else {
      return Ok(None);
    };
    let Some(frontier) = manifest.preview_header_frontier.as_deref() else {
      return Ok(None);
    };
    if frontier != manifest.latest_frontier.as_slice() {
      return Ok(None); // stale header — the caller falls back.
    }
    let Some(header) = headers.iter().find(|header| header.frontier == frontier) else {
      return Ok(None);
    };
    let projection = decode_chunk::<crate::loro_projection::ProjectionBlocks>(&header.bytes, "preview header payload")?;
    let mut document = crate::loro_projection::document_from_projection_blocks(projection);
    if document.ids.document_id == 0 {
      document.ids.document_id = manifest.document_id;
    }
    document.frontier = frontier.to_vec();
    Ok(Some(document))
  }

  pub fn write(&self, path: impl AsRef<Path>) -> io::Result<()> {
    let path = path.as_ref();
    reject_foreign_package_path(path)?;
    fidelity::event(FidelityClass::Persistence, "write", || {
      format!(
        "snapshots={} segments={} revisions={} assets={} frontier={}",
        self.loro_snapshots.len(),
        self.loro_update_segments.len(),
        self.revisions.len(),
        self.assets.len(),
        frontier_tag(&self.manifest.latest_frontier)
      )
    });
    let payload = self.to_bytes()?;
    if file_has_journal_header(path)? {
      let bytes = fs::read(path)?;
      let (transactions, committed_end) = committed_journal_transactions(&bytes)?;
      let rewrite = committed_end != bytes.len()
        || transactions.len() >= JOURNAL_GENERATION_COMPACTION_THRESHOLD
        || bytes.len() > journal_transaction_len(payload.len()).saturating_mul(4);
      if rewrite {
        write_journal_generation(path, &payload)
      } else {
        append_journal_transaction(path, &payload)
      }
    } else {
      write_journal_generation(path, &payload)
    }
  }

  pub fn append_latest_update_to_path(&self, path: impl AsRef<Path>) -> io::Result<()> {
    let path = path.as_ref();
    reject_foreign_package_path(path)?;
    let Some(segment) = self.loro_update_segments.last().cloned() else {
      return self.write(path);
    };
    if !file_has_journal_header(path)? {
      return self.write(path);
    }
    let payload = encode_journal_delta(&PackageJournalDelta::Update {
      manifest: self.manifest.clone(),
      segment,
      preview_header: self.preview_headers.last().cloned(),
    })?;
    append_journal_transaction(path, &payload)
  }

  pub fn append_latest_update_to_prepared_path(&self, path: impl AsRef<Path>) -> io::Result<()> {
    self.append_latest_update_to_prepared_path_with_durability(path, true)
  }

  /// §act-twelve A12.5.1: the repair-persist lane. Canonical REPAIRS are
  /// re-derivable (a crash that loses one re-detects the defect and repairs
  /// convergently on the next projection — the whole §P2a design), so their
  /// journal append may skip the per-transaction `sync_all` that dominated
  /// heal cost on contended disks. The journal's transaction framing already
  /// drops a torn unsynced tail at replay, and ANY later synced append
  /// (edits, checkpoints) syncs the whole file, covering earlier lazy
  /// transactions.
  pub fn append_latest_update_to_prepared_path_lazy(&self, path: impl AsRef<Path>) -> io::Result<()> {
    self.append_latest_update_to_prepared_path_with_durability(path, false)
  }

  fn append_latest_update_to_prepared_path_with_durability(&self, path: impl AsRef<Path>, sync: bool) -> io::Result<()> {
    let path = path.as_ref();
    reject_foreign_package_path(path)?;
    let Some(segment) = self.loro_update_segments.last().cloned() else {
      return self.write(path);
    };
    if !file_has_journal_header(path)? {
      return self.write(path);
    }
    let payload = encode_journal_delta(&PackageJournalDelta::Update {
      manifest: self.manifest.clone(),
      segment,
      preview_header: self.preview_headers.last().cloned(),
    })?;
    append_journal_transaction_to_prepared_file_with_durability(path, &payload, sync)
  }

  pub fn append_assets_to_path(&self, path: impl AsRef<Path>) -> io::Result<()> {
    let path = path.as_ref();
    reject_foreign_package_path(path)?;
    if !file_has_journal_header(path)? {
      return self.write(path);
    }
    let payload = encode_journal_delta(&PackageJournalDelta::Assets {
      manifest: self.manifest.clone(),
      assets: self.assets.clone(),
    })?;
    append_journal_transaction(path, &payload)
  }

  pub fn from_bytes(bytes: &[u8]) -> io::Result<Self> {
    if bytes.starts_with(JOURNAL_MAGIC) {
      return Self::from_journal_bytes(bytes);
    }
    Self::from_compact_bytes(bytes)
  }

  fn from_journal_bytes(bytes: &[u8]) -> io::Result<Self> {
    let probe_t0 = std::time::Instant::now();
    let payloads = committed_journal_payloads(bytes)?;
    let probe_scan = probe_t0.elapsed();
    // Decode from the LAST full generation: everything before it is
    // superseded, and decoding every stacked generation made opening a large
    // journal (~5 × 86MB on the impact-doc package) tens of seconds.
    let latest_generation = payloads
      .iter()
      .rposition(|payload| payload.starts_with(PACKAGE_MAGIC))
      .unwrap_or(0);
    let mut package = None;
    for payload in &payloads[latest_generation..] {
      let payload = *payload;
      if payload.starts_with(PACKAGE_MAGIC) {
        package = Some(Self::from_compact_bytes(payload)?);
        continue;
      }
      let delta = decode_journal_delta(payload)?;
      let current = package
        .as_mut()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Flowstate package journal delta precedes a full generation"))?;
      match delta {
        PackageJournalDelta::Update {
          manifest,
          segment,
          preview_header,
        } => {
          if !current
            .loro_update_segments
            .iter()
            .any(|existing| existing.segment_id == segment.segment_id)
          {
            current.loro_update_segments.push(segment);
          }
          current.manifest = manifest;
          if current.manifest.projection_cache_frontier.is_none() {
            current.projection_caches.clear();
          }
          if current.manifest.search_cache_frontier.is_none() {
            current.search_units.clear();
          }
          // §act-four M2: adopt the delta's fresh preview header (edited docs
          // keep an up-to-date header even without a full checkpoint).
          if let Some(header) = preview_header {
            current.preview_headers = vec![header];
          }
        },
        PackageJournalDelta::Assets { manifest, assets } => {
          current.manifest = manifest;
          current.assets = assets;
        },
      }
    }
    let mut package =
      package.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Flowstate package journal has no complete full generation"))?;
    // §19: journal deltas append update segments after the base generation, so
    // rebuild the integrity index over the reconstructed chunk set before
    // validating it for consistency. The manifest's own segment/asset indexes
    // arrive with the delta and remain checked by `validate_manifest_indexes`.
    package.integrity_index = package.build_integrity_index();
    let probe_t1 = std::time::Instant::now();
    package.validate()?;
    if open_probe() {
      eprintln!(
        "[flowstate-read-probe] journal_scan+hash={probe_scan:?} generations={} journal_validate={:?}",
        payloads.len(),
        probe_t1.elapsed()
      );
    }
    Ok(package)
  }

  fn from_compact_bytes(bytes: &[u8]) -> io::Result<Self> {
    // Decode each chunk straight from its slice into the owned structs — the
    // former shape copied every payload into an intermediate `Vec<u8>` first,
    // a full extra pass over a multi-hundred-MB package on every open.
    let probe_t0 = std::time::Instant::now();
    let chunks = read_chunk_slices_deferring(bytes, |_| true, |kind| kind == CHUNK_LORO_SNAPSHOT)?;
    let probe_slices = probe_t0.elapsed();
    let probe_t1 = std::time::Instant::now();
    let mut manifest = None;
    let mut deferred_snapshot_checksums = std::collections::HashMap::new();
    let mut loro_snapshots = Vec::new();
    let mut loro_update_segments = Vec::new();
    let mut assets = Vec::new();
    let mut revisions = Vec::new();
    let mut projection_caches = Vec::new();
    let mut search_units = Vec::new();
    let mut thumbnails = Vec::new();
    let mut integrity_index = Vec::new();
    let mut preview_headers = Vec::new();
    let mut shallow_snapshots = Vec::new();

    for (kind, payload, deferred_checksum) in chunks {
      match kind {
        CHUNK_MANIFEST => manifest = Some(decode_chunk(payload, "manifest")?),
        CHUNK_LORO_SNAPSHOT => {
          let snapshot: LoroSnapshotChunk = decode_chunk(payload, "Loro snapshot")?;
          if let Some(checksum) = deferred_checksum {
            deferred_snapshot_checksums.insert(snapshot.snapshot_id, checksum);
          }
          loro_snapshots.push(snapshot);
        },
        CHUNK_LORO_UPDATE_SEGMENT => loro_update_segments.push(decode_chunk(payload, "Loro update segment")?),
        CHUNK_ASSET => assets.push(decode_chunk(payload, "asset")?),
        CHUNK_REVISION_INDEX => revisions.push(decode_chunk(payload, "revision")?),
        CHUNK_PROJECTION_CACHE => projection_caches.push(decode_chunk(payload, "projection cache")?),
        CHUNK_SEARCH_UNIT => search_units.push(decode_chunk(payload, "search unit")?),
        CHUNK_THUMBNAIL => thumbnails.push(decode_chunk(payload, "thumbnail")?),
        CHUNK_INTEGRITY_INDEX => integrity_index.push(decode_chunk(payload, "integrity index entry")?),
        CHUNK_PREVIEW_HEADER => preview_headers.push(decode_chunk(payload, "preview header")?),
        CHUNK_SHALLOW_SNAPSHOT => shallow_snapshots.push(decode_chunk(payload, "shallow Loro snapshot")?),
        _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "unknown Flowstate package chunk kind")),
      }
    }

    let package = Self {
      manifest: manifest.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Flowstate package has no manifest"))?,
      loro_snapshots,
      loro_update_segments,
      assets,
      revisions,
      projection_caches,
      search_units,
      thumbnails,
      integrity_index,
      preview_headers,
      shallow_snapshots,
      deferred_snapshot_checksums,
    };
    let probe_decode = probe_t1.elapsed();
    let probe_t2 = std::time::Instant::now();
    package.validate()?;
    if open_probe() {
      eprintln!(
        "[flowstate-read-probe] table+hash={probe_slices:?} chunk_decode={probe_decode:?} validate={:?}",
        probe_t2.elapsed()
      );
    }
    Ok(package)
  }

  pub fn to_bytes(&self) -> io::Result<Vec<u8>> {
    // §A12.1.3 slice 2b: a write mints FRESH table checksums over the
    // in-memory payloads — verify any read-deferred snapshot hashes first so
    // on-disk corruption can never be laundered into a "valid" package.
    for snapshot in &self.loro_snapshots {
      self.verify_snapshot_chunk(snapshot)?;
    }
    self.validate()?;
    let mut chunks = Vec::new();
    chunks.push(Chunk {
      kind: CHUNK_MANIFEST,
      payload: encode_chunk(&self.manifest, "manifest")?,
    });
    for snapshot in &self.loro_snapshots {
      chunks.push(Chunk {
        kind: CHUNK_LORO_SNAPSHOT,
        payload: encode_chunk(snapshot, "Loro snapshot")?,
      });
    }
    for segment in &self.loro_update_segments {
      chunks.push(Chunk {
        kind: CHUNK_LORO_UPDATE_SEGMENT,
        payload: encode_chunk(segment, "Loro update segment")?,
      });
    }
    for asset in &self.assets {
      chunks.push(Chunk {
        kind: CHUNK_ASSET,
        payload: encode_chunk(asset, "asset")?,
      });
    }
    for revision in &self.revisions {
      chunks.push(Chunk {
        kind: CHUNK_REVISION_INDEX,
        payload: encode_chunk(revision, "revision")?,
      });
    }
    for cache in &self.projection_caches {
      chunks.push(Chunk {
        kind: CHUNK_PROJECTION_CACHE,
        payload: encode_chunk(cache, "projection cache")?,
      });
    }
    for header in &self.preview_headers {
      chunks.push(Chunk {
        kind: CHUNK_PREVIEW_HEADER,
        payload: encode_chunk(header, "preview header")?,
      });
    }
    for shallow in &self.shallow_snapshots {
      chunks.push(Chunk {
        kind: CHUNK_SHALLOW_SNAPSHOT,
        payload: encode_chunk(shallow, "shallow Loro snapshot")?,
      });
    }
    for unit in &self.search_units {
      chunks.push(Chunk {
        kind: CHUNK_SEARCH_UNIT,
        payload: encode_chunk(unit, "search unit")?,
      });
    }
    for thumbnail in &self.thumbnails {
      chunks.push(Chunk {
        kind: CHUNK_THUMBNAIL,
        payload: encode_chunk(thumbnail, "thumbnail")?,
      });
    }
    for entry in &self.integrity_index {
      chunks.push(Chunk {
        kind: CHUNK_INTEGRITY_INDEX,
        payload: encode_chunk(entry, "integrity index entry")?,
      });
    }
    write_chunks(&chunks)
  }

  /// Delta-scoped validation for [`Self::append_update_segment`]: the new
  /// segment must chain from the previous persisted tip, carry a correct
  /// checksum, and land the manifest on its `to` frontier. Everything already
  /// stored was validated when it entered the package.
  fn validate_appended_segment(&self) -> io::Result<()> {
    if !package_format_version_supported(self.manifest.package_format_version) {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "unsupported Flowstate package format version"));
    }
    if self.manifest.loro_schema_version != LORO_SCHEMA_VERSION {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "unsupported Flowstate Loro schema version"));
    }
    let segment = self
      .loro_update_segments
      .last()
      .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "appended update segment is missing"))?;
    if segment.checksum != blake3_hash(&segment.bytes) {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "Loro update segment checksum mismatch"));
    }
    validate_frontiers(&segment.from_frontier, "update segment from frontier")?;
    validate_version_vector(&segment.from_version_vector, "update segment from version vector")?;
    validate_frontiers(&segment.to_frontier, "update segment to frontier")?;
    validate_version_vector(&segment.to_version_vector, "update segment to version vector")?;
    if segment.to_frontier != self.manifest.latest_frontier {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "latest update segment frontier does not match manifest",
      ));
    }
    let previous_tip = if self.loro_update_segments.len() >= 2 {
      &self.loro_update_segments[self.loro_update_segments.len() - 2].to_frontier
    } else {
      let snapshot = self
        .latest_snapshot()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "latest Loro snapshot is missing"))?;
      &snapshot.frontier
    };
    if &segment.from_frontier != previous_tip {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "appended update segment does not chain from the persisted tip",
      ));
    }
    Ok(())
  }

  pub fn validate(&self) -> io::Result<()> {
    if !package_format_version_supported(self.manifest.package_format_version) {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "unsupported Flowstate package format version"));
    }
    if self.manifest.loro_schema_version != LORO_SCHEMA_VERSION {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "unsupported Flowstate Loro schema version"));
    }
    let snapshot = self
      .latest_snapshot()
      .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "latest Loro snapshot is missing"))?;
    if self.loro_update_segments.is_empty() && snapshot.frontier != self.manifest.latest_frontier {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "latest snapshot frontier does not match manifest",
      ));
    }
    if let Some(last_segment) = self.loro_update_segments.last()
      && last_segment.to_frontier != self.manifest.latest_frontier
    {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "latest update segment frontier does not match manifest",
      ));
    }
    validate_frontiers(&self.manifest.latest_frontier, "manifest latest frontier")?;
    validate_version_vector(&self.manifest.latest_version_vector, "manifest latest version vector")?;
    if let Some(frontier) = &self.manifest.projection_cache_frontier {
      validate_frontiers(frontier, "manifest projection cache frontier")?;
      if !self
        .projection_caches
        .iter()
        .any(|cache| cache.frontier == *frontier)
      {
        return Err(io::Error::new(
          io::ErrorKind::InvalidData,
          "manifest projection cache frontier has no cache chunk",
        ));
      }
    }
    if let Some(frontier) = &self.manifest.search_cache_frontier {
      validate_frontiers(frontier, "manifest search cache frontier")?;
    }
    for snapshot in &self.loro_snapshots {
      validate_frontiers(&snapshot.frontier, "snapshot frontier")?;
      validate_version_vector(&snapshot.version_vector, "snapshot version vector")?;
      if snapshot.bytes.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "empty Loro snapshot bytes"));
      }
    }
    for revision in &self.revisions {
      validate_frontiers(&revision.frontier, "revision frontier")?;
      validate_version_vector(&revision.version_vector, "revision version vector")?;
    }
    for shallow in &self.shallow_snapshots {
      validate_frontiers(&shallow.root_frontier, "shallow snapshot root frontier")?;
      validate_version_vector(&shallow.root_version_vector, "shallow snapshot root version vector")?;
      validate_frontiers(&shallow.frontier, "shallow snapshot frontier")?;
      validate_version_vector(&shallow.version_vector, "shallow snapshot version vector")?;
      if shallow.bytes.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "empty shallow Loro snapshot bytes"));
      }
    }
    if let Some(stamp) = self.manifest.shallow_snapshot_frontier.as_deref()
      && !self
        .shallow_snapshots
        .iter()
        .any(|shallow| shallow.frontier == stamp)
    {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "manifest names a shallow snapshot frontier with no matching chunk",
      ));
    }
    for cache in &self.projection_caches {
      validate_frontiers(&cache.frontier, "projection cache frontier")?;
      // Structural decode of the cache payload is DEFERRED to its consumer
      // (`current_projection_document`, which already fails soft to a full
      // materialization on a bad payload): decoding the whole document model
      // here made every routine validate — package open, save, append — pay
      // seconds on a large document.
    }
    for thumbnail in &self.thumbnails {
      validate_frontiers(&thumbnail.frontier, "thumbnail frontier")?;
      if thumbnail.bytes.is_empty() || thumbnail.mime_type.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid empty thumbnail chunk"));
      }
      if let Some(revision_id) = thumbnail.revision_id
        && !self
          .revisions
          .iter()
          .any(|revision| revision.revision_id == revision_id)
      {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "thumbnail references an unknown revision"));
      }
    }
    if self
      .loro_update_segments
      .iter()
      .any(|segment| segment.checksum != blake3_hash(&segment.bytes))
    {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "Loro update segment checksum mismatch"));
    }
    for segment in &self.loro_update_segments {
      validate_frontiers(&segment.from_frontier, "update segment from frontier")?;
      validate_version_vector(&segment.from_version_vector, "update segment from version vector")?;
      validate_frontiers(&segment.to_frontier, "update segment to frontier")?;
      validate_version_vector(&segment.to_version_vector, "update segment to version vector")?;
    }
    let mut expected_frontier = snapshot.frontier.as_slice();
    let mut expected_version_vector = snapshot.version_vector.as_slice();
    for segment in &self.loro_update_segments {
      if segment.from_frontier != expected_frontier {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "Loro update segment frontier chain is broken"));
      }
      if segment.from_version_vector != expected_version_vector {
        return Err(io::Error::new(
          io::ErrorKind::InvalidData,
          "Loro update segment version-vector chain is broken",
        ));
      }
      expected_frontier = &segment.to_frontier;
      expected_version_vector = &segment.to_version_vector;
    }
    if self
      .assets
      .iter()
      .any(|asset| asset.content_hash != blake3_hash(&asset.bytes) || asset.byte_length != asset.bytes.len() as u64)
    {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "asset hash or length mismatch"));
    }
    self.validate_manifest_indexes()?;
    self.validate_integrity_index()?;
    self.validate_schema_migrations()?;
    // NOT validated here: a full `load_loro_doc_unvalidated()` round-trip.
    // Importing the whole Loro snapshot as a validation step made every
    // routine validate — open, save, compaction, and (via append) every
    // keystroke — pay a multi-second CRDT import on a large document (the
    // .db8 first-edit freeze). Corruption in the snapshot bytes still
    // surfaces at the one place the doc is actually loaded.
    Ok(())
  }

  pub fn rebuild_projection_cache_from_loro(&mut self, doc: &LoroDoc) -> io::Result<()> {
    doc.commit();
    let frontier = encode_frontiers(&doc.state_frontiers());
    let projection = crate::loro_projection::projection_blocks_from_loro(doc)?;
    // §act-four M2: the preview header is the leading blocks of the same
    // materialization — O(K) to trim + serialize on top of the full cache.
    self.write_preview_header(&frontier, &projection)?;
    // §act-ten A10.5: `manifest.block_boundaries` is NOT persisted anymore. It
    // fed only `cold_viewport_blocks`, which has no production caller and its
    // own lazy derive fallback — while `body_block_boundaries` re-ran BOTH
    // whole-registry scans the projection walk above just performed, per
    // checkpoint (~every 900ms while typing; the pegged package thread).
    self.projection_caches.clear();
    self.projection_caches.push(ProjectionCacheChunk {
      frontier: frontier.clone(),
      bytes: encode_chunk(&projection, "projection cache payload")?,
    });
    self.manifest.projection_cache_frontier = Some(frontier);
    self.manifest.modified_at_unix_secs = unix_time_secs();
    self.validate()?;
    Ok(())
  }

  /// §act-five P9: rebuild BOTH the projection cache and the search units from a
  /// SINGLE `document_from_loro` materialization. The separate
  /// `rebuild_projection_cache_from_loro` + `rebuild_search_units_from_loro`
  /// calls each ran a full projection walk — a double materialization per
  /// checkpoint that pegged the off-gate package thread during collaboration.
  /// The search units derive from the SAME materialized blocks the cache does.
  /// §A13.4.1: cache rebuild that uses the runtime's MAINTAINED projection
  /// when it matches the doc frontier — skipping the O(doc)
  /// `document_from_loro` walk (365ms of a routine checkpoint on the impact
  /// doc). Mismatch (or `FLOWSTATE_DISABLE_PROJECTION_CACHE_FASTPATH`) falls
  /// back to the from-Loro walk; `FLOWSTATE_CHECKPOINT_CACHE_VERIFY` runs
  /// both and asserts payload equality.
  pub fn rebuild_caches_from_projection(&mut self, doc: &LoroDoc, document: &crate::DocumentProjection) -> io::Result<()> {
    doc.commit();
    static DISABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let disabled = *DISABLED.get_or_init(|| std::env::var_os("FLOWSTATE_DISABLE_PROJECTION_CACHE_FASTPATH").is_some());
    let frontier = encode_frontiers(&doc.state_frontiers());
    if disabled || document.frontier != frontier {
      return self.rebuild_caches_from_loro(doc);
    }
    let probe_t = std::time::Instant::now();
    let projection = projection_blocks_from_document(document);
    if open_probe() {
      eprintln!("[flowstate-cache-probe] projection_convert={:?}", probe_t.elapsed());
    }
    static VERIFY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *VERIFY.get_or_init(|| std::env::var_os("FLOWSTATE_CHECKPOINT_CACHE_VERIFY").is_some()) {
      let from_loro = crate::loro_projection::projection_blocks_from_loro(doc)?;
      let ours = encode_chunk(&projection, "projection cache payload")?;
      let theirs = encode_chunk(&from_loro, "projection cache payload")?;
      assert!(
        ours == theirs,
        "[flowstate-checkpoint-cache-verify] projection-built cache payload diverges from the from-Loro walk"
      );
    }
    self.finish_cache_rebuild(doc, frontier, projection)
  }

  pub fn rebuild_caches_from_loro(&mut self, doc: &LoroDoc) -> io::Result<()> {
    doc.commit();
    let frontier = encode_frontiers(&doc.state_frontiers());
    let projection = crate::loro_projection::projection_blocks_from_loro(doc)?;
    self.finish_cache_rebuild(doc, frontier, projection)
  }

  fn finish_cache_rebuild(&mut self, doc: &LoroDoc, frontier: Vec<u8>, projection: crate::loro_projection::ProjectionBlocks) -> io::Result<()> {
    let probe_t = std::time::Instant::now();
    self.write_preview_header(&frontier, &projection)?;
    let probe_preview = probe_t.elapsed();
    let probe_t = std::time::Instant::now();
    // §act-ten A10.5: block-boundary persistence removed (see
    // `rebuild_projection_cache_from_loro`).
    self.search_units = crate::package_search::search_units_from_input_blocks(doc, &projection.blocks, self.manifest.document_id, &frontier)?;
    let probe_search = probe_t.elapsed();
    let probe_t = std::time::Instant::now();
    self.manifest.search_cache_frontier = Some(frontier.clone());
    self.projection_caches.clear();
    self.projection_caches.push(ProjectionCacheChunk {
      frontier: frontier.clone(),
      bytes: encode_chunk(&projection, "projection cache payload")?,
    });
    let probe_encode = probe_t.elapsed();
    let probe_t = std::time::Instant::now();
    self.manifest.projection_cache_frontier = Some(frontier);
    self.manifest.modified_at_unix_secs = unix_time_secs();
    self.validate()?;
    if open_probe() {
      eprintln!(
        "[flowstate-cache-probe] preview={probe_preview:?} search={probe_search:?} encode={probe_encode:?} validate={:?}",
        probe_t.elapsed()
      );
    }
    Ok(())
  }

  /// §act-four M2: (re)write the preview header from a materialized
  /// [`ProjectionBlocks`] — the leading `PREVIEW_HEADER_BLOCKS` blocks + their
  /// ids, serialized into a tiny chunk at `frontier`. Idempotent: replaces any
  /// existing header. The header is written on every checkpoint so cold preview
  /// is always fresh and `O(viewport)`, independent of the full cache.
  fn write_preview_header(&mut self, frontier: &[u8], projection: &crate::loro_projection::ProjectionBlocks) -> io::Result<()> {
    let block_count = projection.blocks.len().min(PREVIEW_HEADER_BLOCKS);
    // `paragraph_ids` is parallel to the PARAGRAPH blocks in order; count how
    // many of the leading blocks are paragraphs to trim the ids in lockstep.
    let paragraph_count = projection.blocks[..block_count]
      .iter()
      .filter(|block| matches!(block, crate::InputBlock::Paragraph(_)))
      .count();
    let header = crate::loro_projection::ProjectionBlocks {
      document_id: projection.document_id,
      blocks: projection.blocks[..block_count].to_vec(),
      paragraph_ids: projection.paragraph_ids[..paragraph_count.min(projection.paragraph_ids.len())].to_vec(),
      block_ids: projection.block_ids[..block_count.min(projection.block_ids.len())].to_vec(),
      sections: Vec::new(),
    };
    self.preview_headers.clear();
    self.preview_headers.push(PreviewHeaderChunk {
      frontier: frontier.to_vec(),
      bytes: encode_chunk(&header, "preview header payload")?,
    });
    self.manifest.preview_header_frontier = Some(frontier.to_vec());
    Ok(())
  }

  /// §act-four M2: refresh the preview header from a LIVE, already-materialized
  /// projection (the runtime's), keyed to the package's current latest
  /// frontier — `O(PREVIEW_HEADER_BLOCKS)`, no re-materialization. Called on
  /// every edit-persist so the header stays fresh even between full checkpoints
  /// (it then rides the journal delta to disk). The caller guarantees the
  /// projection reflects `latest_frontier`.
  pub fn refresh_preview_header(&mut self, projection: &crate::DocumentProjection) -> io::Result<()> {
    let frontier = self.manifest.latest_frontier.clone();
    let header_blocks = preview_header_blocks_from_document(projection, PREVIEW_HEADER_BLOCKS);
    self.preview_headers.clear();
    self.preview_headers.push(PreviewHeaderChunk {
      frontier: frontier.clone(),
      bytes: encode_chunk(&header_blocks, "preview header payload")?,
    });
    self.manifest.preview_header_frontier = Some(frontier);
    Ok(())
  }

  pub fn current_projection_document(&self) -> io::Result<Option<crate::DocumentProjection>> {
    let Some(frontier) = self.manifest.projection_cache_frontier.as_deref() else {
      return Ok(None);
    };
    if frontier != self.manifest.latest_frontier.as_slice() {
      return Ok(None);
    }
    let Some(cache) = self
      .projection_caches
      .iter()
      .find(|cache| cache.frontier == frontier)
    else {
      return Ok(None);
    };
    let projection = decode_chunk::<crate::loro_projection::ProjectionBlocks>(&cache.bytes, "projection cache payload")?;
    let mut document = crate::loro_projection::document_from_projection_blocks(projection);
    if document.ids.document_id == 0 {
      document.ids.document_id = self.manifest.document_id;
    }
    document.frontier = frontier.to_vec();
    Ok(Some(document))
  }

  fn validate_manifest_indexes(&self) -> io::Result<()> {
    if self.manifest.update_segment_index.len() != self.loro_update_segments.len() {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "manifest update segment index length mismatch",
      ));
    }
    for (index, segment) in self
      .manifest
      .update_segment_index
      .iter()
      .zip(&self.loro_update_segments)
    {
      if index.id != segment.segment_id || index.checksum != segment.checksum || index.byte_length != segment.bytes.len() as u64 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "manifest update segment index mismatch"));
      }
    }
    if self.manifest.asset_index.len() != self.assets.len() {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "manifest asset index length mismatch"));
    }
    for (index, asset) in self.manifest.asset_index.iter().zip(&self.assets) {
      if index.id != asset.asset_id || index.checksum != asset.content_hash || index.byte_length != asset.byte_length {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "manifest asset index mismatch"));
      }
    }
    Ok(())
  }

  /// §19: cross-check the named integrity index against the actual durable
  /// chunks. An absent index (older packages) is accepted for backward
  /// compatibility; a present index must be complete and consistent.
  fn validate_integrity_index(&self) -> io::Result<()> {
    // §19: an empty integrity index is the legacy compatibility case for older
    // packages that predate the named index. Preserve that behavior explicitly.
    if self.integrity_index.is_empty() {
      return Ok(());
    }
    let expected = self.build_integrity_index();
    if self.integrity_index.len() != expected.len() {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "integrity index entry count mismatch"));
    }
    let expected = Self::integrity_index_key_set(&expected)?;
    let actual = Self::integrity_index_key_set(&self.integrity_index)?;
    if actual != expected {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "integrity index entries do not match durable chunks",
      ));
    }
    for entry in &self.integrity_index {
      let actual = match entry.chunk_kind {
        CHUNK_LORO_SNAPSHOT => self
          .loro_snapshots
          .iter()
          .find(|snapshot| snapshot.snapshot_id == entry.id)
          .map(|snapshot| (self.integrity_checksum_for_snapshot(snapshot), snapshot.bytes.len() as u64)),
        CHUNK_LORO_UPDATE_SEGMENT => self
          .loro_update_segments
          .iter()
          .find(|segment| segment.segment_id == entry.id)
          .map(|segment| (segment.checksum, segment.bytes.len() as u64)),
        CHUNK_ASSET => self
          .assets
          .iter()
          .find(|asset| asset.asset_id == entry.id)
          .map(|asset| (asset.content_hash, asset.byte_length)),
        CHUNK_SHALLOW_SNAPSHOT => self
          .shallow_snapshots
          .iter()
          .find(|shallow| shallow.snapshot_id == entry.id)
          .map(|shallow| (blake3_hash(&shallow.bytes), shallow.bytes.len() as u64)),
        _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "integrity index has an unknown chunk kind")),
      };
      let Some((checksum, byte_length)) = actual else {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "integrity index references a missing chunk"));
      };
      if entry.checksum != checksum || entry.byte_length != byte_length {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "integrity index checksum or length mismatch"));
      }
    }
    Ok(())
  }

  /// §27: validate the schema migration log. Empty at schema v1. Each record
  /// must describe a forward migration whose target does not exceed this
  /// package's schema version.
  fn validate_schema_migrations(&self) -> io::Result<()> {
    for migration in &self.manifest.schema_migrations {
      if migration.from_version >= migration.to_version || migration.to_version > self.manifest.loro_schema_version {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "inconsistent schema migration record"));
      }
    }
    Ok(())
  }

  /// §27: record a schema migration event in package metadata. The migration's
  /// Loro document mutation (if any) must be committed separately as an ordinary
  /// Loro change with the `"migration"` origin before/after calling this.
  pub fn record_schema_migration(&mut self, from_version: u32, to_version: u32, description: impl Into<String>) -> io::Result<u128> {
    if from_version >= to_version || to_version > self.manifest.loro_schema_version {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "schema migration must move forward to a version supported by this package",
      ));
    }
    let migration_id = Uuid::new_v4().as_u128();
    self.manifest.schema_migrations.push(SchemaMigrationRecord {
      id: migration_id,
      from_version,
      to_version,
      applied_at_unix_secs: unix_time_secs(),
      description: description.into(),
    });
    self.manifest.modified_at_unix_secs = unix_time_secs();
    self.validate()?;
    Ok(migration_id)
  }

  pub fn latest_snapshot(&self) -> Option<&LoroSnapshotChunk> {
    self
      .loro_snapshots
      .iter()
      .find(|snapshot| snapshot.snapshot_id == self.manifest.latest_snapshot_id)
  }

  fn snapshot_for_frontier(&self, frontier: &[u8]) -> Option<&LoroSnapshotChunk> {
    self
      .loro_snapshots
      .iter()
      .find(|snapshot| snapshot.frontier == frontier)
  }

  fn with_manifest_indexes(mut self) -> io::Result<Self> {
    self.refresh_manifest_indexes();
    Ok(self)
  }

  /// Rebuild the manifest's update-segment/asset indexes and the §19 integrity
  /// index from the package's current durable chunks. Infallible and idempotent.
  /// Public wrapper for external maintenance tooling (e.g. restoring a
  /// package to its latest snapshot after stripping segments).
  pub fn refresh_manifest_indexes_public(&mut self) {
    self.refresh_manifest_indexes();
  }

  fn refresh_manifest_indexes(&mut self) {
    let mut update_segment_index = Vec::with_capacity(self.loro_update_segments.len());
    for segment in &self.loro_update_segments {
      update_segment_index.push(ChunkRef {
        id: segment.segment_id,
        checksum: segment.checksum,
        byte_length: segment.bytes.len() as u64,
      });
    }
    let mut asset_index = Vec::with_capacity(self.assets.len());
    for asset in &self.assets {
      asset_index.push(ChunkRef {
        id: asset.asset_id,
        checksum: asset.content_hash,
        byte_length: asset.byte_length,
      });
    }
    self.manifest.update_segment_index = update_segment_index;
    self.manifest.asset_index = asset_index;
    self.integrity_index = self.build_integrity_index();
  }

  /// §19: build a complete integrity index covering every durable chunk — Loro
  /// snapshots, update segments, and assets — with each chunk's id, kind, BLAKE3
  /// checksum, and byte length.
  fn build_integrity_index(&self) -> Vec<IntegrityIndexEntry> {
    let mut entries = Vec::with_capacity(self.loro_snapshots.len() + self.loro_update_segments.len() + self.assets.len());
    for snapshot in &self.loro_snapshots {
      entries.push(IntegrityIndexEntry {
        chunk_kind: CHUNK_LORO_SNAPSHOT,
        id: snapshot.snapshot_id,
        checksum: self.integrity_checksum_for_snapshot(snapshot),
        byte_length: snapshot.bytes.len() as u64,
      });
    }
    for segment in &self.loro_update_segments {
      entries.push(IntegrityIndexEntry {
        chunk_kind: CHUNK_LORO_UPDATE_SEGMENT,
        id: segment.segment_id,
        checksum: segment.checksum,
        byte_length: segment.bytes.len() as u64,
      });
    }
    for asset in &self.assets {
      entries.push(IntegrityIndexEntry {
        chunk_kind: CHUNK_ASSET,
        id: asset.asset_id,
        checksum: asset.content_hash,
        byte_length: asset.byte_length,
      });
    }
    for shallow in &self.shallow_snapshots {
      entries.push(IntegrityIndexEntry {
        chunk_kind: CHUNK_SHALLOW_SNAPSHOT,
        id: shallow.snapshot_id,
        checksum: blake3_hash(&shallow.bytes),
        byte_length: shallow.bytes.len() as u64,
      });
    }
    entries
  }

  fn integrity_index_key_set(entries: &[IntegrityIndexEntry]) -> io::Result<HashSet<(u32, u128)>> {
    let mut keys = HashSet::with_capacity(entries.len());
    for entry in entries {
      if !keys.insert((entry.chunk_kind, entry.id)) {
        return Err(io::Error::new(
          io::ErrorKind::InvalidData,
          "integrity index contains duplicate chunk identity",
        ));
      }
    }
    Ok(keys)
  }
}

pub fn read_loro_db8(path: impl AsRef<Path>) -> io::Result<LoroDoc> {
  DocumentPackage::read(path)?.load_loro_doc()
}

pub fn write_loro_db8(path: impl AsRef<Path>, doc: &LoroDoc, title: &str) -> io::Result<()> {
  DocumentPackage::from_loro_snapshot(doc, title)?.write(path)
}

pub fn loro_db8_bytes(doc: &LoroDoc, title: &str) -> io::Result<Vec<u8>> {
  DocumentPackage::from_loro_snapshot(doc, title)?.to_bytes()
}

/// Parse the package header + chunk table and return `(kind, payload)` SLICES
/// into `bytes` for every chunk `keep` selects, verifying the table checksum
/// of each kept chunk. Bounds are validated for EVERY chunk (kept or not);
/// checksums are only computed for kept ones — a preview that needs two small
/// chunks must not BLAKE3 an entire multi-hundred-MB package. Zero payload
/// copies happen here; callers decode straight from the returned slices.
fn read_chunk_slices(bytes: &[u8], keep: impl FnMut(u32) -> bool) -> io::Result<Vec<(u32, &[u8])>> {
  Ok(
    read_chunk_slices_deferring(bytes, keep, |_| false)?
      .into_iter()
      .map(|(kind, payload, _)| (kind, payload))
      .collect(),
  )
}

/// §A12.1.3 slice 2b: like [`read_chunk_slices`], but chunk kinds selected by
/// `defer_hash` skip the BLAKE3 verification here and return their TABLE
/// checksum instead — the consumer that actually decodes those bytes must
/// verify first ([`DocumentPackage::verify_snapshot_chunk`]). This moves the
/// dominant open cost (hashing the multi-MB full snapshot chunk) off the
/// open path and onto its rare consumers (sidecar/full loads).
/// One chunk from [`read_chunk_slices_deferring`]: kind, payload slice, and —
/// for hash-deferred kinds — the unverified chunk-table checksum.
type DeferredChunkSlice<'bytes> = (u32, &'bytes [u8], Option<[u8; 32]>);

fn read_chunk_slices_deferring(
  bytes: &[u8],
  mut keep: impl FnMut(u32) -> bool,
  defer_hash: impl Fn(u32) -> bool,
) -> io::Result<Vec<DeferredChunkSlice<'_>>> {
  let mut cursor = Cursor::new(bytes);
  let mut magic = [0_u8; 16];
  cursor.read_exact(&mut magic)?;
  if &magic != PACKAGE_MAGIC {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid Flowstate Loro package magic"));
  }
  let version = read_u32(&mut cursor)?;
  if version != PACKAGE_HEADER_VERSION {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "unsupported Flowstate package header version"));
  }
  let chunk_count = read_u32(&mut cursor)?;
  if usize::try_from(chunk_count).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "package chunk count overflows usize"))?
    > MAX_PACKAGE_CHUNK_COUNT
  {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "too many package chunks"));
  }
  let mut entries = Vec::with_capacity(chunk_count as usize);
  for _ in 0..chunk_count {
    let kind = read_u32(&mut cursor)?;
    let offset = read_u64(&mut cursor)?;
    let len = read_u64(&mut cursor)?;
    let mut checksum = [0_u8; 32];
    cursor.read_exact(&mut checksum)?;
    entries.push(ChunkEntry { kind, offset, len, checksum });
  }
  let mut chunks = Vec::with_capacity(entries.len());
  for entry in entries {
    let start = usize::try_from(entry.offset).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "chunk offset overflows usize"))?;
    let len = usize::try_from(entry.len).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "chunk length overflows usize"))?;
    if len > MAX_PACKAGE_CHUNK_COUNT.saturating_mul(PACKAGE_CHUNK_TABLE_ENTRY_BYTES) {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "package chunk length is unreasonably large"));
    }
    let end = start
      .checked_add(len)
      .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "chunk range overflows usize"))?;
    if end > bytes.len() {
      return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "Flowstate package chunk is truncated"));
    }
    if !keep(entry.kind) {
      continue;
    }
    let payload = &bytes[start..end];
    if defer_hash(entry.kind) {
      chunks.push((entry.kind, payload, Some(entry.checksum)));
      continue;
    }
    if blake3_hash(payload) != entry.checksum {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "Flowstate package chunk checksum mismatch"));
    }
    chunks.push((entry.kind, payload, None));
  }
  Ok(chunks)
}

fn cached_search_units_from_compact_bytes(bytes: &[u8]) -> io::Result<(DocumentPackageManifest, Vec<SearchUnitChunk>)> {
  // Selective read: only the manifest + search-unit chunks are decoded (and
  // checksum-verified); snapshots/segments/assets are neither hashed nor
  // copied — this consumer only wants the search cache.
  let chunks = read_chunk_slices(bytes, |kind| matches!(kind, CHUNK_MANIFEST | CHUNK_SEARCH_UNIT))?;
  let mut manifest = None;
  let mut search_units = Vec::new();
  for (kind, payload) in chunks {
    match kind {
      CHUNK_MANIFEST => manifest = Some(decode_chunk(payload, "manifest")?),
      CHUNK_SEARCH_UNIT => search_units.push(decode_chunk(payload, "search unit")?),
      _ => {},
    }
  }
  let manifest = manifest.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Flowstate package has no manifest"))?;
  Ok((manifest, search_units))
}

/// The preview triple: manifest + projection caches + assets.
type CachedProjectionParts = (DocumentPackageManifest, Vec<ProjectionCacheChunk>, Vec<AssetChunk>);

/// Preview fast path (§P3): decode ONLY the manifest, the frontier-matched
/// projection cache, and the asset chunks from raw package bytes — skipping
/// snapshot/segment decode, their BLAKE3 sweeps, and `validate()` entirely.
/// Returns `Ok(None)` when no frontier-current projection cache exists (the
/// caller falls back to the full package read + Loro materialization).
fn cached_projection_from_compact_bytes(bytes: &[u8]) -> io::Result<Option<CachedProjectionParts>> {
  let chunks = read_chunk_slices(bytes, |kind| matches!(kind, CHUNK_MANIFEST | CHUNK_PROJECTION_CACHE | CHUNK_ASSET))?;
  let mut manifest: Option<DocumentPackageManifest> = None;
  let mut caches = Vec::new();
  let mut assets = Vec::new();
  for (kind, payload) in chunks {
    match kind {
      CHUNK_MANIFEST => manifest = Some(decode_chunk(payload, "manifest")?),
      CHUNK_PROJECTION_CACHE => caches.push(decode_chunk(payload, "projection cache")?),
      CHUNK_ASSET => assets.push(decode_chunk(payload, "asset")?),
      _ => {},
    }
  }
  let manifest = manifest.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Flowstate package has no manifest"))?;
  Ok(Some((manifest, caches, assets)))
}

/// §act-four M2: build a trimmed [`ProjectionBlocks`] from the leading
/// `k` blocks of a live projection — `O(k)`, the header's payload.
/// §A13.4.1: the FULL `DocumentProjection → ProjectionBlocks` converter —
/// the checkpoint's cache payload built from the projection the runtime
/// already maintains, instead of a whole-doc `document_from_loro` walk.
/// Paragraph text comes through the ranged reader (projection runs hold
/// lengths only); object blocks self-carry content; `sections` copy
/// wholesale (canonical page attrs ride the maintained projection).
fn projection_blocks_from_document(document: &crate::DocumentProjection) -> crate::loro_projection::ProjectionBlocks {
  let block_count = document.blocks.len();
  let mut blocks = Vec::with_capacity(block_count);
  let mut paragraph_ix = 0usize;
  for block in document.blocks.range(0..block_count) {
    match block {
      crate::Block::Paragraph(_) => {
        blocks.push(crate::InputBlock::Paragraph(crate::input_paragraph_from_document_range(
          document,
          paragraph_ix,
          0..usize::MAX,
        )));
        paragraph_ix += 1;
      },
      other => blocks.push(crate::input_block_from_block(other)),
    }
  }
  crate::loro_projection::ProjectionBlocks {
    document_id: document.ids.document_id,
    blocks,
    paragraph_ids: document.ids.paragraph_ids.as_ref().clone(),
    block_ids: document.ids.block_ids.as_ref().clone(),
    sections: document.sections.as_ref().clone(),
  }
}

fn preview_header_blocks_from_document(document: &crate::DocumentProjection, k: usize) -> crate::loro_projection::ProjectionBlocks {
  let block_count = document.blocks.len().min(k);
  // Paragraph blocks must carry their TEXT (a `Block::Paragraph`'s runs hold
  // only lengths — the text lives in the document rope), so read each paragraph
  // through the ranged reader; object blocks carry their content directly.
  let mut blocks = Vec::with_capacity(block_count);
  let mut paragraph_ix = 0usize;
  for block in document.blocks.range(0..block_count) {
    match block {
      crate::Block::Paragraph(_) => {
        blocks.push(crate::InputBlock::Paragraph(crate::input_paragraph_from_document_range(
          document,
          paragraph_ix,
          0..usize::MAX,
        )));
        paragraph_ix += 1;
      },
      other => blocks.push(crate::input_block_from_block(other)),
    }
  }
  crate::loro_projection::ProjectionBlocks {
    document_id: document.ids.document_id,
    blocks,
    paragraph_ids: document.ids.paragraph_ids[..paragraph_ix.min(document.ids.paragraph_ids.len())].to_vec(),
    block_ids: document.ids.block_ids[..block_count.min(document.ids.block_ids.len())].to_vec(),
    sections: Vec::new(),
  }
}

/// The preview-header pair: manifest + preview header chunks.
type PreviewHeaderParts = (DocumentPackageManifest, Vec<PreviewHeaderChunk>);

/// §act-four M2: decode ONLY the manifest + preview-header chunk(s) from raw
/// compact bytes — skipping every heavy chunk and the `validate()` sweep.
/// §act-ten A10.13: positioned preview-header extraction — never materializes
/// the whole file. Journal: walk transaction headers by seek (magic + len +
/// checksum reads only), sniff each payload's leading magic, then read the
/// last full generation SELECTIVELY (chunk table + manifest/preview chunks)
/// followed by the small delta payloads after it. Compact: selective chunks at
/// offset 0. Chunk checksums still verify everything actually read; the
/// journal transaction checksum (whole-payload) is NOT re-verified here — the
/// COMMIT marker gates torn tails, and preview is a fallback-tolerant read.
fn preview_header_positioned(path: &Path) -> io::Result<Option<PreviewHeaderParts>> {
  use std::io::{Read as _, Seek as _, SeekFrom};
  let mut file = fs::File::open(path)?;
  let file_len = file.metadata()?.len();
  let mut magic = [0_u8; 16];
  file.read_exact(&mut magic)?;
  if &magic == PACKAGE_MAGIC {
    return preview_header_from_compact_positioned(&mut file, 0, file_len);
  }
  if &magic != JOURNAL_MAGIC {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid Flowstate package journal header"));
  }
  let mut version = [0_u8; 4];
  file.read_exact(&mut version)?;
  if u32::from_le_bytes(version) != JOURNAL_HEADER_VERSION {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "unsupported Flowstate package journal version",
    ));
  }
  // Walk committed transactions: record (payload_offset, payload_len, is_generation).
  let mut offset = (JOURNAL_MAGIC.len() + 4) as u64;
  let mut committed: Vec<(u64, u64, bool)> = Vec::new();
  let fixed_len = (JOURNAL_TXN_MAGIC.len() + 8 + 32) as u64;
  loop {
    if file_len.saturating_sub(offset) < fixed_len {
      break;
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut txn_magic = [0_u8; 8];
    file.read_exact(&mut txn_magic)?;
    if &txn_magic != JOURNAL_TXN_MAGIC {
      break;
    }
    let mut len_bytes = [0_u8; 8];
    file.read_exact(&mut len_bytes)?;
    let payload_len = u64::from_le_bytes(len_bytes);
    let payload_offset = offset + fixed_len;
    let commit_end = match payload_offset
      .checked_add(payload_len)
      .and_then(|end| end.checked_add(JOURNAL_COMMIT_MAGIC.len() as u64))
    {
      Some(end) if end <= file_len => end,
      _ => break,
    };
    file.seek(SeekFrom::Start(commit_end - JOURNAL_COMMIT_MAGIC.len() as u64))?;
    let mut commit_magic = [0_u8; 8];
    file.read_exact(&mut commit_magic)?;
    if &commit_magic != JOURNAL_COMMIT_MAGIC {
      break;
    }
    // Sniff the payload's leading magic to classify generation vs delta.
    file.seek(SeekFrom::Start(payload_offset))?;
    let mut payload_magic = [0_u8; 16];
    let is_generation = payload_len >= 16 && {
      file.read_exact(&mut payload_magic)?;
      &payload_magic == PACKAGE_MAGIC
    };
    committed.push((payload_offset, payload_len, is_generation));
    offset = commit_end;
  }
  let latest_generation = committed
    .iter()
    .rposition(|(_, _, is_generation)| *is_generation)
    .unwrap_or(0);
  let mut extracted: Option<PreviewHeaderParts> = None;
  for &(payload_offset, payload_len, is_generation) in &committed[latest_generation..] {
    if is_generation {
      extracted = preview_header_from_compact_positioned(&mut file, payload_offset, payload_len)?;
      continue;
    }
    // Delta payloads are the small per-edit appends — read them whole.
    file.seek(SeekFrom::Start(payload_offset))?;
    let mut payload = vec![
      0_u8;
      usize::try_from(payload_len)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "journal payload length overflows usize"))?
    ];
    file.read_exact(&mut payload)?;
    let delta = decode_journal_delta(&payload)?;
    let Some((manifest, headers)) = extracted.as_mut() else {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "Flowstate package journal delta precedes a full generation",
      ));
    };
    if let PackageJournalDelta::Update {
      manifest: next_manifest,
      preview_header,
      ..
    } = delta
    {
      *manifest = next_manifest;
      if let Some(header) = preview_header {
        *headers = vec![header];
      }
    }
  }
  Ok(extracted)
}

/// Selective chunk read of a compact package image at `base..base+len` inside
/// `file`: chunk table first, then pread of ONLY the manifest + preview-header
/// chunks (checksum-verified).
fn preview_header_from_compact_positioned(file: &mut fs::File, base: u64, region_len: u64) -> io::Result<Option<PreviewHeaderParts>> {
  use std::io::{Read as _, Seek as _, SeekFrom};
  file.seek(SeekFrom::Start(base))?;
  let mut magic = [0_u8; 16];
  file.read_exact(&mut magic)?;
  if &magic != PACKAGE_MAGIC {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid Flowstate Loro package magic"));
  }
  let mut word = [0_u8; 4];
  file.read_exact(&mut word)?;
  if u32::from_le_bytes(word) != PACKAGE_HEADER_VERSION {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "unsupported Flowstate package header version"));
  }
  file.read_exact(&mut word)?;
  let chunk_count = u32::from_le_bytes(word);
  if chunk_count as usize > MAX_PACKAGE_CHUNK_COUNT {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "too many package chunks"));
  }
  let mut table = vec![0_u8; chunk_count as usize * PACKAGE_CHUNK_TABLE_ENTRY_BYTES];
  file.read_exact(&mut table)?;
  let mut manifest: Option<DocumentPackageManifest> = None;
  let mut headers = Vec::new();
  for entry in table.chunks_exact(PACKAGE_CHUNK_TABLE_ENTRY_BYTES) {
    let kind = u32::from_le_bytes(entry[0..4].try_into().expect("sized"));
    if !matches!(kind, CHUNK_MANIFEST | CHUNK_PREVIEW_HEADER) {
      continue;
    }
    let chunk_offset = u64::from_le_bytes(entry[4..12].try_into().expect("sized"));
    let chunk_len = u64::from_le_bytes(entry[12..20].try_into().expect("sized"));
    let checksum: [u8; 32] = entry[20..52].try_into().expect("sized");
    if chunk_len as usize > MAX_PACKAGE_CHUNK_COUNT.saturating_mul(PACKAGE_CHUNK_TABLE_ENTRY_BYTES) {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "package chunk length is unreasonably large"));
    }
    let end = chunk_offset
      .checked_add(chunk_len)
      .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "chunk range overflows"))?;
    if end > region_len {
      return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "Flowstate package chunk is truncated"));
    }
    file.seek(SeekFrom::Start(base + chunk_offset))?;
    let mut payload =
      vec![0_u8; usize::try_from(chunk_len).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "chunk length overflows usize"))?];
    file.read_exact(&mut payload)?;
    if blake3_hash(&payload) != checksum {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "Flowstate package chunk checksum mismatch"));
    }
    match kind {
      CHUNK_MANIFEST => manifest = Some(decode_chunk(&payload, "manifest")?),
      CHUNK_PREVIEW_HEADER => headers.push(decode_chunk(&payload, "preview header")?),
      _ => {},
    }
  }
  let manifest = manifest.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Flowstate package has no manifest"))?;
  Ok(Some((manifest, headers)))
}

fn preview_header_from_compact_bytes(bytes: &[u8]) -> io::Result<Option<PreviewHeaderParts>> {
  let chunks = read_chunk_slices(bytes, |kind| matches!(kind, CHUNK_MANIFEST | CHUNK_PREVIEW_HEADER))?;
  let mut manifest: Option<DocumentPackageManifest> = None;
  let mut headers = Vec::new();
  for (kind, payload) in chunks {
    match kind {
      CHUNK_MANIFEST => manifest = Some(decode_chunk(payload, "manifest")?),
      CHUNK_PREVIEW_HEADER => headers.push(decode_chunk(payload, "preview header")?),
      _ => {},
    }
  }
  let manifest = manifest.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Flowstate package has no manifest"))?;
  Ok(Some((manifest, headers)))
}

/// Journal-aware sibling: decode the last full generation's header, then fold
/// each trailing `Update` delta's fresh header (an edited doc's latest header
/// rides its journal delta).
fn preview_header_from_journal_bytes(bytes: &[u8]) -> io::Result<Option<PreviewHeaderParts>> {
  let payloads = committed_journal_payloads(bytes)?;
  let latest_generation = payloads
    .iter()
    .rposition(|payload| payload.starts_with(PACKAGE_MAGIC))
    .unwrap_or(0);
  let mut extracted: Option<PreviewHeaderParts> = None;
  for payload in &payloads[latest_generation..] {
    let payload = *payload;
    if payload.starts_with(PACKAGE_MAGIC) {
      extracted = preview_header_from_compact_bytes(payload)?;
      continue;
    }
    let delta = decode_journal_delta(payload)?;
    let Some((manifest, headers)) = extracted.as_mut() else {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "Flowstate package journal delta precedes a full generation",
      ));
    };
    if let PackageJournalDelta::Update {
      manifest: next_manifest,
      preview_header,
      ..
    } = delta
    {
      *manifest = next_manifest;
      if let Some(header) = preview_header {
        *headers = vec![header];
      }
    }
  }
  Ok(extracted)
}

fn cached_search_units_from_journal_bytes(bytes: &[u8]) -> io::Result<(DocumentPackageManifest, Vec<SearchUnitChunk>)> {
  let payloads = committed_journal_payloads(bytes)?;
  // Decode from the LAST full generation only — everything earlier is
  // superseded (same law as `from_journal_bytes`); decoding every stacked
  // generation re-parsed the whole journal history.
  let latest_generation = payloads
    .iter()
    .rposition(|payload| payload.starts_with(PACKAGE_MAGIC))
    .unwrap_or(0);
  let mut cached = None;
  for payload in &payloads[latest_generation..] {
    let payload = *payload;
    if payload.starts_with(PACKAGE_MAGIC) {
      cached = Some(cached_search_units_from_compact_bytes(payload)?);
      continue;
    }
    let delta = decode_journal_delta(payload)?;
    let Some((manifest, search_units)) = cached.as_mut() else {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "Flowstate package journal delta precedes a full generation",
      ));
    };
    match delta {
      PackageJournalDelta::Update { manifest: next_manifest, .. } | PackageJournalDelta::Assets { manifest: next_manifest, .. } => {
        *manifest = next_manifest;
        if manifest.search_cache_frontier.is_none() {
          search_units.clear();
        }
      },
    }
  }
  cached.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Flowstate package journal has no complete full generation"))
}

/// Journal-aware sibling of [`cached_projection_from_compact_bytes`]: decode
/// the last full generation's manifest/projection-cache/assets, then fold the
/// trailing deltas' manifests (an Update after the cached frontier makes the
/// cache stale, which the caller detects via the frontier mismatch).
fn cached_projection_from_journal_bytes(bytes: &[u8]) -> io::Result<Option<CachedProjectionParts>> {
  let payloads = committed_journal_payloads(bytes)?;
  let latest_generation = payloads
    .iter()
    .rposition(|payload| payload.starts_with(PACKAGE_MAGIC))
    .unwrap_or(0);
  let mut cached = None;
  for payload in &payloads[latest_generation..] {
    let payload = *payload;
    if payload.starts_with(PACKAGE_MAGIC) {
      cached = cached_projection_from_compact_bytes(payload)?;
      continue;
    }
    let delta = decode_journal_delta(payload)?;
    let Some((manifest, caches, assets)) = cached.as_mut() else {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "Flowstate package journal delta precedes a full generation",
      ));
    };
    match delta {
      PackageJournalDelta::Update { manifest: next_manifest, .. } => {
        *manifest = next_manifest;
        if manifest.projection_cache_frontier.is_none() {
          caches.clear();
        }
      },
      PackageJournalDelta::Assets {
        manifest: next_manifest,
        assets: next_assets,
      } => {
        *manifest = next_manifest;
        *assets = next_assets;
      },
    }
  }
  Ok(cached)
}

fn write_chunks(chunks: &[Chunk]) -> io::Result<Vec<u8>> {
  let table_len = chunks.len() * (4 + 8 + 8 + 32);
  let header_len = PACKAGE_MAGIC.len() + 4 + 4 + table_len;
  let payload_len = chunks
    .iter()
    .map(|chunk| chunk.payload.len())
    .sum::<usize>();
  let mut bytes = Vec::with_capacity(header_len + payload_len);
  bytes.extend_from_slice(PACKAGE_MAGIC);
  write_u32(&mut bytes, PACKAGE_HEADER_VERSION);
  write_u32(
    &mut bytes,
    u32::try_from(chunks.len()).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "too many package chunks"))?,
  );
  let mut offset = header_len;
  for chunk in chunks {
    write_u32(&mut bytes, chunk.kind);
    write_u64(&mut bytes, offset as u64);
    write_u64(&mut bytes, chunk.payload.len() as u64);
    bytes.extend_from_slice(&blake3_hash(&chunk.payload));
    offset += chunk.payload.len();
  }
  for chunk in chunks {
    bytes.extend_from_slice(&chunk.payload);
  }
  Ok(bytes)
}

fn encode_chunk<T: Serialize>(value: &T, label: &'static str) -> io::Result<Vec<u8>> {
  postcard::to_stdvec(value).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, format!("encoding {label} failed: {error}")))
}

fn decode_chunk<'a, T: Deserialize<'a>>(bytes: &'a [u8], label: &'static str) -> io::Result<T> {
  if bytes.len() > MAX_PACKAGE_CHUNK_COUNT.saturating_mul(PACKAGE_CHUNK_TABLE_ENTRY_BYTES) {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      format!("decoding {label} failed: chunk too large"),
    ));
  }
  postcard::from_bytes(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, format!("decoding {label} failed: {error}")))
}

fn encode_frontiers(frontiers: &Frontiers) -> Vec<u8> {
  frontiers.encode()
}

fn decode_frontiers(bytes: &[u8]) -> io::Result<Frontiers> {
  Frontiers::decode(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, format!("decoding frontiers failed: {error}")))
}

fn encode_version_vector(version_vector: &VersionVector) -> Vec<u8> {
  version_vector.encode()
}

/// Resolve a frontier that must lie within `doc`'s history into its version
/// vector directly on the live doc, replacing the `fork_at` (O(doc)) pattern
/// that previously existed only to read the fork's `state_vv`.
fn frontiers_version_vector(doc: &LoroDoc, frontiers: &Frontiers) -> io::Result<VersionVector> {
  doc.frontiers_to_vv(frontiers).ok_or_else(|| {
    io::Error::new(
      io::ErrorKind::InvalidData,
      "revision frontier is not contained in the Loro document history",
    )
  })
}

fn validate_frontiers(bytes: &[u8], label: &'static str) -> io::Result<()> {
  Frontiers::decode(bytes)
    .map(|_| ())
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, format!("decoding {label} failed: {error}")))
}

fn validate_version_vector(bytes: &[u8], label: &'static str) -> io::Result<()> {
  VersionVector::decode(bytes)
    .map(|_| ())
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, format!("decoding {label} failed: {error}")))
}

fn blake3_hash(bytes: &[u8]) -> [u8; 32] {
  // §A13.1.2: multithread the hash for multi-MB payloads (3-6GB/s vs ~1);
  // below the threshold the rayon fan-out costs more than it saves.
  const RAYON_HASH_MIN_BYTES: usize = 1024 * 1024;
  if bytes.len() >= RAYON_HASH_MIN_BYTES {
    let mut hasher = blake3::Hasher::new();
    hasher.update_rayon(bytes);
    return *hasher.finalize().as_bytes();
  }
  *blake3::hash(bytes).as_bytes()
}

/// Compact `<len>B:<hex-prefix>` tag for an encoded frontier / version vector.
/// Used only inside lazy fidelity detail closures, so it never runs unless
/// tracing is enabled; it truncates to keep firehose lines short.
fn frontier_tag(bytes: &[u8]) -> String {
  use std::fmt::Write as _;
  let mut hex = String::new();
  for byte in bytes.iter().take(12) {
    let _ = write!(hex, "{byte:02x}");
  }
  if bytes.len() > 12 {
    hex.push('~');
  }
  format!("{}B:{hex}", bytes.len())
}

fn loro_io_error(error: impl std::error::Error + Send + Sync + 'static) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, error)
}

fn unix_time_secs() -> i64 {
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map_or(0, |duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
}

fn read_u32(cursor: &mut Cursor<&[u8]>) -> io::Result<u32> {
  let mut bytes = [0; 4];
  cursor.read_exact(&mut bytes)?;
  Ok(u32::from_le_bytes(bytes))
}

fn write_u32(bytes: &mut Vec<u8>, value: u32) {
  bytes.extend_from_slice(&value.to_le_bytes());
}

fn read_u64(cursor: &mut Cursor<&[u8]>) -> io::Result<u64> {
  let mut bytes = [0; 8];
  cursor.read_exact(&mut bytes)?;
  Ok(u64::from_le_bytes(bytes))
}

fn write_u64(bytes: &mut Vec<u8>, value: u64) {
  bytes.extend_from_slice(&value.to_le_bytes());
}

/// Extensions that name a foreign *source / export* document format — never a
/// Flowstate package. Writing package/journal bytes onto one of these would
/// clobber a user's source file (the 2026-07-08 docx-overwrite data-loss bug,
/// where a phase-V open leaked the source `.docx` path into the autosave
/// target). The package writers below refuse it, turning any future regression
/// of that class into a safe error instead of silent data loss.
///
/// Export to `.docx`/`.pdf` is UNAFFECTED: it goes through the docx/pdf writers
/// (`write_docx` / `write_pdf_with_db8_bytes`) and the app's own
/// `write_bytes_to_path`, none of which call these package methods.
fn reject_foreign_package_path(path: &Path) -> io::Result<()> {
  if let Some(extension) = path.extension().and_then(|extension| extension.to_str()) {
    let extension = extension.to_ascii_lowercase();
    if matches!(extension.as_str(), "docx" | "doc" | "docm" | "dotx" | "pdf" | "rtf" | "odt") {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
          "refusing to write a Flowstate package to a foreign document path (would overwrite the source): {}",
          path.display()
        ),
      ));
    }
  }
  Ok(())
}

fn file_has_journal_header(path: &Path) -> io::Result<bool> {
  let mut file = match fs::File::open(path) {
    Ok(file) => file,
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
    Err(error) => return Err(error),
  };
  let mut magic = [0_u8; 16];
  match file.read_exact(&mut magic) {
    Ok(()) => Ok(&magic == JOURNAL_MAGIC),
    Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
    Err(error) => Err(error),
  }
}

fn append_journal_transaction(path: &Path, payload: &[u8]) -> io::Result<()> {
  let parent = path
    .parent()
    .filter(|parent| !parent.as_os_str().is_empty())
    .unwrap_or_else(|| Path::new("."));
  fs::create_dir_all(parent)?;
  let existing = fs::read(path)?;
  let (_, committed_end) = committed_journal_transactions(&existing)?;
  let mut file = OpenOptions::new().read(true).write(true).open(path)?;
  if committed_end != existing.len() {
    file.set_len(committed_end as u64)?;
  }
  file.seek(SeekFrom::End(0))?;
  let mut bytes = Vec::with_capacity(journal_transaction_len(payload.len()));
  append_journal_transaction_bytes(&mut bytes, payload);
  file.write_all(&bytes)?;
  file.sync_all()
}

fn append_journal_transaction_to_prepared_file(path: &Path, payload: &[u8]) -> io::Result<()> {
  append_journal_transaction_to_prepared_file_with_durability(path, payload, true)
}

fn append_journal_transaction_to_prepared_file_with_durability(path: &Path, payload: &[u8], sync: bool) -> io::Result<()> {
  let parent = path
    .parent()
    .filter(|parent| !parent.as_os_str().is_empty())
    .unwrap_or_else(|| Path::new("."));
  fs::create_dir_all(parent)?;
  let mut file = OpenOptions::new().append(true).open(path)?;
  let mut bytes = Vec::with_capacity(journal_transaction_len(payload.len()));
  append_journal_transaction_bytes(&mut bytes, payload);
  file.write_all(&bytes)?;
  if sync {
    file.sync_all()?;
  }
  Ok(())
}

fn append_journal_transaction_bytes(bytes: &mut Vec<u8>, payload: &[u8]) {
  bytes.extend_from_slice(JOURNAL_TXN_MAGIC);
  write_u64(bytes, payload.len() as u64);
  bytes.extend_from_slice(&blake3_hash(payload));
  bytes.extend_from_slice(payload);
  bytes.extend_from_slice(JOURNAL_COMMIT_MAGIC);
}

fn journal_transaction_len(payload_len: usize) -> usize {
  JOURNAL_TXN_MAGIC.len() + 8 + 32 + payload_len + JOURNAL_COMMIT_MAGIC.len()
}

fn committed_journal_payloads(bytes: &[u8]) -> io::Result<Vec<&[u8]>> {
  committed_journal_transactions(bytes).map(|(payloads, _)| payloads)
}

fn committed_journal_transactions(bytes: &[u8]) -> io::Result<(Vec<&[u8]>, usize)> {
  if bytes.len() < JOURNAL_MAGIC.len() + 4 || !bytes.starts_with(JOURNAL_MAGIC) {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid Flowstate package journal header"));
  }
  let mut cursor = Cursor::new(&bytes[JOURNAL_MAGIC.len()..]);
  let version = read_u32(&mut cursor)?;
  if version != JOURNAL_HEADER_VERSION {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "unsupported Flowstate package journal version",
    ));
  }
  let mut offset = JOURNAL_MAGIC.len() + 4;
  let mut committed = Vec::new();
  while offset < bytes.len() {
    let fixed_len = JOURNAL_TXN_MAGIC.len() + 8 + 32;
    if bytes.len().saturating_sub(offset) < fixed_len {
      break;
    }
    if &bytes[offset..offset + JOURNAL_TXN_MAGIC.len()] != JOURNAL_TXN_MAGIC {
      break;
    }
    offset += JOURNAL_TXN_MAGIC.len();
    let payload_len = u64::from_le_bytes(
      bytes[offset..offset + 8]
        .try_into()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid Flowstate journal transaction length"))?,
    );
    offset += 8;
    let checksum: [u8; 32] = bytes[offset..offset + 32]
      .try_into()
      .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid Flowstate journal transaction checksum"))?;
    offset += 32;
    let payload_len = usize::try_from(payload_len)
      .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Flowstate journal transaction length overflows usize"))?;
    let payload_end = match offset.checked_add(payload_len) {
      Some(end) => end,
      None => break,
    };
    let commit_end = match payload_end.checked_add(JOURNAL_COMMIT_MAGIC.len()) {
      Some(end) => end,
      None => break,
    };
    if commit_end > bytes.len() {
      break;
    }
    if &bytes[payload_end..commit_end] != JOURNAL_COMMIT_MAGIC {
      break;
    }
    let payload = &bytes[offset..payload_end];
    // §A13.1: a full-generation payload (starts with PACKAGE_MAGIC) skips
    // the whole-payload hash — its integrity is layered inside it (chunk
    // table BLAKE3 per chunk, deferred kinds verify-at-decode), and a torn
    // append is caught structurally (length bound + commit magic above).
    // Delta payloads stay hashed: they are small and carry no internal
    // checksums. `FLOWSTATE_EAGER_JOURNAL_HASH` restores full hashing.
    static EAGER_HASH: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let eager = *EAGER_HASH.get_or_init(|| std::env::var_os("FLOWSTATE_EAGER_JOURNAL_HASH").is_some());
    if (eager || !payload.starts_with(PACKAGE_MAGIC)) && blake3_hash(payload) != checksum {
      break;
    }
    committed.push(payload);
    offset = commit_end;
  }
  if committed.is_empty() {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "Flowstate package journal has no complete transaction",
    ));
  }
  Ok((committed, offset))
}

fn write_journal_generation(path: &Path, payload: &[u8]) -> io::Result<()> {
  let mut bytes = Vec::with_capacity(JOURNAL_MAGIC.len() + 4 + journal_transaction_len(payload.len()));
  bytes.extend_from_slice(JOURNAL_MAGIC);
  write_u32(&mut bytes, JOURNAL_HEADER_VERSION);
  append_journal_transaction_bytes(&mut bytes, payload);
  write_bytes_atomic(path, &bytes)
}

fn encode_journal_delta(delta: &PackageJournalDelta) -> io::Result<Vec<u8>> {
  let encoded = encode_chunk(delta, "package journal delta")?;
  let mut payload = Vec::with_capacity(JOURNAL_DELTA_MAGIC.len() + encoded.len());
  payload.extend_from_slice(JOURNAL_DELTA_MAGIC);
  payload.extend_from_slice(&encoded);
  Ok(payload)
}

fn decode_journal_delta(payload: &[u8]) -> io::Result<PackageJournalDelta> {
  let encoded = payload
    .strip_prefix(JOURNAL_DELTA_MAGIC)
    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "unknown Flowstate package journal transaction"))?;
  decode_chunk(encoded, "package journal delta")
}

fn package_map_string(map: &loro::LoroMap, key: &str) -> Option<String> {
  match map.get(key)? {
    ValueOrContainer::Value(LoroValue::String(value)) => Some(value.to_string()),
    _ => None,
  }
}

fn loro_revision_exists(doc: &LoroDoc, revision_id: u128) -> bool {
  let root = doc.get_map(crate::loro_schema::ROOT);
  let Some(ValueOrContainer::Container(Container::List(revisions))) = root.get(crate::loro_schema::REVISIONS) else {
    return false;
  };
  for index in 0..revisions.len() {
    let Some(ValueOrContainer::Container(Container::Map(revision))) = revisions.get(index) else {
      continue;
    };
    if package_map_string(&revision, "id").and_then(|id| id.parse::<u128>().ok()) == Some(revision_id) {
      return true;
    }
  }
  false
}

fn package_map_binary(map: &loro::LoroMap, key: &str) -> Option<Vec<u8>> {
  match map.get(key)? {
    ValueOrContainer::Value(LoroValue::Binary(value)) => Some(value.to_vec()),
    _ => None,
  }
}

fn package_map_i64(map: &loro::LoroMap, key: &str) -> Option<i64> {
  match map.get(key)? {
    ValueOrContainer::Value(LoroValue::I64(value)) => Some(value),
    _ => None,
  }
}

fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
  let parent = path
    .parent()
    .filter(|p| !p.as_os_str().is_empty())
    .unwrap_or_else(|| Path::new("."));
  fs::create_dir_all(parent)?;
  atomicwrites::AtomicFile::new(path, atomicwrites::AllowOverwrite)
    .write(|file| file.write_all(bytes))
    .map_err(Into::into)
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;

  use super::*;
  use crate::{
    AssetId, AssetRecord, Block, CellId, ColumnId, InputBlock, InputBlockAlignment, InputEquationBlock, InputEquationDisplay,
    InputEquationSyntax, InputImageBlock, InputImageSizing, InputParagraph, InputRun, InputTableBlock, InputTableCell, InputTableCellBlock,
    InputTableColumn, InputTableColumnWidth, InputTableRow, InputTableStyle, RowId, RunStyles, TableCellBlock, document_from_loro,
    document_to_loro,
    loro_schema::{body_text, new_loro_document},
    read_db8_bytes,
  };
  use loro::{Container, LoroDoc, LoroMap, LoroValue, ValueOrContainer};

  #[test]
  fn package_roundtrips_loro_snapshot() -> io::Result<()> {
    let doc = new_loro_document("Roundtrip").map_err(loro_test_error)?;
    let text = body_text(&doc);
    text
      .insert(text.len_unicode(), "Hello Loro")
      .map_err(loro_test_error)?;
    let bytes = loro_db8_bytes(&doc, "Roundtrip")?;

    let package = DocumentPackage::from_bytes(&bytes)?;
    assert_eq!(package.manifest.package_format_version, LORO_PACKAGE_FORMAT_VERSION);
    assert_eq!(package.manifest.loro_schema_version, LORO_SCHEMA_VERSION);
    assert_eq!(package.loro_snapshots.len(), 2);
    assert_eq!(
      package.manifest.projection_cache_frontier.as_deref(),
      Some(package.manifest.latest_frontier.as_slice())
    );
    assert_eq!(package.projection_caches.len(), 1);

    let loaded = package.load_loro_doc()?;
    assert_eq!(body_text(&loaded).to_string(), "\nHello Loro");
    let projected = package
      .current_projection_document()?
      .expect("projection cache");
    assert_eq!(crate::paragraph_text(&projected, 0), "Hello Loro");
    Ok(())
  }

  #[test]
  fn package_read_repairs_an_incomplete_journal_tail() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("tail-recovery.db8");
    let doc = new_loro_document("Tail recovery").map_err(loro_test_error)?;
    let package = DocumentPackage::from_loro_snapshot(&doc, "Tail recovery")?;
    package.write(&path)?;
    let committed_len = fs::metadata(&path)?.len();

    let mut file = OpenOptions::new().append(true).open(&path)?;
    std::io::Write::write_all(&mut file, b"incomplete journal transaction")?;
    drop(file);
    assert!(fs::metadata(&path)?.len() > committed_len);

    let repaired = DocumentPackage::read(&path)?;

    repaired.validate()?;
    assert_eq!(fs::metadata(&path)?.len(), committed_len);
    Ok(())
  }

  #[test]
  fn package_write_refuses_to_clobber_a_foreign_source_document() -> io::Result<()> {
    // §data-loss regression (2026-07-08): a phase-V open leaked the source
    // `.docx` path into the autosave target, so a checkpoint wrote a `.db8`
    // journal ON TOP of the user's original docx. The guard makes every package
    // writer refuse a foreign-format path, so this can never silently destroy a
    // source file again — regardless of how the path was derived upstream.
    let dir = tempfile::tempdir()?;
    let doc = new_loro_document("Guard").map_err(loro_test_error)?;
    let package = DocumentPackage::from_loro_snapshot(&doc, "Guard")?;

    // A pre-existing "source" docx (arbitrary non-package bytes) must survive.
    let docx_path = dir.path().join("source.docx");
    let original = b"PK\x03\x04 pretend this is a real docx zip";
    fs::write(&docx_path, original)?;

    for foreign in ["source.docx", "paper.pdf", "brief.doc", "legacy.rtf", "notes.odt"] {
      let path = dir.path().join(foreign);
      let error = package
        .write(&path)
        .expect_err("package write to a foreign path must be rejected");
      assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{foreign}");
    }
    // The append entry points guard identically.
    assert_eq!(
      package
        .append_latest_update_to_path(&docx_path)
        .expect_err("append must reject foreign path")
        .kind(),
      io::ErrorKind::InvalidInput
    );

    // The original docx bytes are untouched — nothing was written.
    assert_eq!(fs::read(&docx_path)?, original);

    // The same package writes cleanly to a real `.db8` path.
    let db8_path = dir.path().join("source.db8");
    package.write(&db8_path)?;
    assert!(file_has_journal_header(&db8_path)?);
    Ok(())
  }

  #[test]
  fn package_rejects_old_final_state_magic() {
    let old_bytes = b"GPTX\x06\0\0\0old-format";
    let error = DocumentPackage::from_bytes(old_bytes).expect_err("old final-state bytes must not load");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
  }

  #[test]
  fn public_db8_reader_rejects_old_final_state_magic() {
    let old_bytes = b"GPTX\x06\0\0\0old-format";
    let error = read_db8_bytes(old_bytes).expect_err("old final-state bytes must not load through public facade");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
  }

  #[test]
  fn public_db8_bytes_roundtrip_through_loro_package() -> io::Result<()> {
    let source = crate::document_from_input_blocks(
      crate::flowstate_document_theme(),
      vec![InputBlock::Paragraph(InputParagraph {
        style: crate::ParagraphStyle::Normal,
        runs: vec![InputRun {
          text: "Hello package".to_string(),
          styles: RunStyles::default(),
        }],
      })],
    );
    let doc = document_to_loro(&source, "Public facade").map_err(loro_test_error)?;
    let bytes =
      DocumentPackage::from_loro_snapshot_with_assets(&doc, "Public facade", crate::loro_import::assets_from_document(&source))?.to_bytes()?;
    let package = DocumentPackage::from_bytes(&bytes)?;
    assert_eq!(package.manifest.package_format_version, LORO_PACKAGE_FORMAT_VERSION);
    let projected = read_db8_bytes(&bytes)?;
    assert_eq!(crate::paragraph_text(&projected, 0), "Hello package");
    Ok(())
  }

  #[test]
  fn package_loads_snapshot_plus_update_segment() -> io::Result<()> {
    let doc = new_loro_document("Append").map_err(loro_test_error)?;
    let mut package = DocumentPackage::from_loro_snapshot(&doc, "Append")?;
    let from_frontier = doc.state_frontiers();
    let from_vv = doc.state_vv();

    let text = body_text(&doc);
    text
      .insert(text.len_unicode(), "after save")
      .map_err(loro_test_error)?;
    doc.commit();
    let update = doc
      .export(ExportMode::updates(&from_vv))
      .map_err(loro_test_error)?;
    package.append_update_segment(&from_frontier, &from_vv, &doc.state_frontiers(), &doc.state_vv(), update)?;
    assert!(package.manifest.projection_cache_frontier.is_none());
    assert!(package.projection_caches.is_empty());

    let bytes = package.to_bytes()?;
    let loaded = DocumentPackage::from_bytes(&bytes)?.load_loro_doc()?;
    assert_eq!(body_text(&loaded).to_string(), "\nafter save");
    Ok(())
  }

  #[test]
  fn package_rejects_manifest_projection_cache_frontier_without_chunk() -> io::Result<()> {
    let doc = new_loro_document("Projection cache").map_err(loro_test_error)?;
    let mut package = DocumentPackage::from_loro_snapshot(&doc, "Projection cache")?;
    package.projection_caches.clear();

    let error = package
      .validate()
      .expect_err("manifest projection cache frontier must point at a cache chunk");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    Ok(())
  }

  #[test]
  fn package_rejects_manifest_update_segment_index_mismatch() -> io::Result<()> {
    let doc = new_loro_document("Append").map_err(loro_test_error)?;
    let mut package = DocumentPackage::from_loro_snapshot(&doc, "Append")?;
    let from_frontier = doc.state_frontiers();
    let from_vv = doc.state_vv();

    body_text(&doc)
      .insert(1, "update")
      .map_err(loro_test_error)?;
    doc.commit();
    let update = doc
      .export(ExportMode::updates(&from_vv))
      .map_err(loro_test_error)?;
    package.append_update_segment(&from_frontier, &from_vv, &doc.state_frontiers(), &doc.state_vv(), update)?;
    package.manifest.update_segment_index[0].byte_length = 0;

    let error = package
      .validate()
      .expect_err("stale manifest segment index must fail validation");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    Ok(())
  }

  #[test]
  fn package_rejects_duplicate_integrity_index_entries() -> io::Result<()> {
    let doc = new_loro_document("Integrity").map_err(loro_test_error)?;
    let mut package = DocumentPackage::from_loro_snapshot(&doc, "Integrity")?;
    package.integrity_index[1].chunk_kind = package.integrity_index[0].chunk_kind;
    package.integrity_index[1].id = package.integrity_index[0].id;

    let error = package
      .validate()
      .expect_err("duplicate integrity entries must fail validation");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    Ok(())
  }

  #[test]
  fn package_replace_assets_is_failure_atomic() -> io::Result<()> {
    let source = crate::document_from_input_blocks(
      crate::flowstate_document_theme(),
      vec![InputBlock::Paragraph(InputParagraph {
        style: crate::ParagraphStyle::Normal,
        runs: vec![InputRun {
          text: "asset".to_string(),
          styles: RunStyles::default(),
        }],
      })],
    );
    let doc = document_to_loro(&source, "Atomic assets").map_err(loro_test_error)?;
    let mut package = DocumentPackage::from_loro_snapshot_with_assets(&doc, "Atomic assets", crate::loro_import::assets_from_document(&source))?;
    let previous_modified_at = package.manifest.modified_at_unix_secs;
    package.manifest.package_format_version = 0;

    let error = package
      .replace_assets_from_document(&source)
      .expect_err("invalid package version must fail validation");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(package.manifest.modified_at_unix_secs, previous_modified_at);
    assert_eq!(package.manifest.package_format_version, 0);
    Ok(())
  }

  #[test]
  fn package_append_update_segment_is_failure_atomic() -> io::Result<()> {
    let doc = new_loro_document("Atomic update").map_err(loro_test_error)?;
    let mut package = DocumentPackage::from_loro_snapshot(&doc, "Atomic update")?;
    let from_frontier = doc.state_frontiers();
    let from_vv = doc.state_vv();
    body_text(&doc).insert(1, "x").map_err(loro_test_error)?;
    doc.commit();
    let update = doc
      .export(ExportMode::updates(&from_vv))
      .map_err(loro_test_error)?;
    let previous_segments = package.loro_update_segments.len();
    let previous_latest_frontier = package.manifest.latest_frontier.clone();
    package.manifest.package_format_version = 0;

    let error = package
      .append_update_segment(&from_frontier, &from_vv, &doc.state_frontiers(), &doc.state_vv(), update)
      .expect_err("invalid package version must fail validation");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(package.loro_update_segments.len(), previous_segments);
    assert_eq!(package.manifest.latest_frontier, previous_latest_frontier);
    assert_eq!(package.manifest.package_format_version, 0);
    Ok(())
  }

  #[test]
  fn package_compact_to_snapshot_is_failure_atomic() -> io::Result<()> {
    let doc = new_loro_document("Atomic compact").map_err(loro_test_error)?;
    let mut package = DocumentPackage::from_loro_snapshot(&doc, "Atomic compact")?;
    let previous_snapshots = package.loro_snapshots.len();
    let previous_update_segments = package.loro_update_segments.len();
    package.manifest.package_format_version = 0;

    let error = package
      .compact_to_snapshot(&doc)
      .expect_err("invalid package version must fail validation");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(package.loro_snapshots.len(), previous_snapshots);
    assert_eq!(package.loro_update_segments.len(), previous_update_segments);
    assert_eq!(package.manifest.package_format_version, 0);
    Ok(())
  }

  #[test]
  fn package_rejects_manifest_asset_index_mismatch() -> io::Result<()> {
    let doc = new_loro_document("Assets").map_err(loro_test_error)?;
    let bytes = b"asset bytes".to_vec();
    let asset = AssetChunk {
      asset_id: 7,
      content_hash: blake3_hash(&bytes),
      mime_type: "application/octet-stream".to_string(),
      byte_length: bytes.len() as u64,
      bytes,
      metadata: Vec::new(),
    };
    let mut package = DocumentPackage::from_loro_snapshot_with_assets(&doc, "Assets", vec![asset])?;
    package.manifest.asset_index[0].checksum = [0; 32];

    let error = package
      .validate()
      .expect_err("stale manifest asset index must fail validation");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    Ok(())
  }

  #[test]
  fn named_revisions_are_restorable_after_compaction() -> io::Result<()> {
    let doc = new_loro_document("Revisions").map_err(loro_test_error)?;
    let mut package = DocumentPackage::from_loro_snapshot(&doc, "Revisions")?;
    let first_revision = package.create_named_revision(&doc, "Blank", "Before edits", None, None)?;

    let from_frontier = doc.state_frontiers();
    let from_vv = doc.state_vv();
    let text = body_text(&doc);
    text.insert(1, "after").map_err(loro_test_error)?;
    doc.commit();
    let update = doc
      .export(ExportMode::updates(&from_vv))
      .map_err(loro_test_error)?;
    package.append_update_segment(&from_frontier, &from_vv, &doc.state_frontiers(), &doc.state_vv(), update)?;
    assert!(package.current_search_units().is_empty());
    let second_revision = package.create_named_revision(&doc, "After", "After text insert", None, None)?;

    package.compact_to_snapshot(&doc)?;

    let first_doc = package.load_revision_loro_doc(first_revision)?;
    assert_eq!(body_text(&first_doc).to_string(), "\n");
    let second_doc = package.load_revision_loro_doc(second_revision)?;
    assert_eq!(body_text(&second_doc).to_string(), "\nafter");
    let latest_doc = package.load_loro_doc()?;
    assert_eq!(body_text(&latest_doc).to_string(), "\nafter");
    Ok(())
  }

  #[test]
  fn revision_chunk_from_snapshot_at_matches_a_fork_at_reference() -> io::Result<()> {
    let doc = new_loro_document("SnapshotAt").map_err(loro_test_error)?;
    let text = body_text(&doc);
    text.insert(1, "first").map_err(loro_test_error)?;
    doc.commit();
    let mut package = DocumentPackage::from_loro_snapshot(&doc, "SnapshotAt")?;

    // Edit past the package's latest frontier so the revision below gets a
    // fresh `snapshot_at` chunk instead of reusing the initial snapshot.
    let from_frontier = doc.state_frontiers();
    let from_vv = doc.state_vv();
    body_text(&doc)
      .insert(body_text(&doc).len_unicode(), " second")
      .map_err(loro_test_error)?;
    doc.commit();
    let update = doc
      .export(ExportMode::updates(&from_vv))
      .map_err(loro_test_error)?;
    package.append_update_segment(&from_frontier, &from_vv, &doc.state_frontiers(), &doc.state_vv(), update)?;

    let revision_frontiers = doc.state_frontiers();
    let revision_id = package.create_named_revision(&doc, "Mid", "After second insert", None, None)?;

    // Advance the live doc past the revision so the chunk genuinely captures
    // an older frontier.
    let from_frontier = doc.state_frontiers();
    let from_vv = doc.state_vv();
    body_text(&doc)
      .insert(body_text(&doc).len_unicode(), " third")
      .map_err(loro_test_error)?;
    doc.commit();
    let update = doc
      .export(ExportMode::updates(&from_vv))
      .map_err(loro_test_error)?;
    package.append_update_segment(&from_frontier, &from_vv, &doc.state_frontiers(), &doc.state_vv(), update)?;

    // The `SnapshotAt` chunk must survive package serialization.
    let package = DocumentPackage::from_bytes(&package.to_bytes()?)?;
    let revision_frontier_bytes = encode_frontiers(&revision_frontiers);
    assert!(
      package
        .snapshot_for_frontier(&revision_frontier_bytes)
        .is_some()
    );

    let revision_doc = package.load_revision_loro_doc(revision_id)?;
    let reference = doc.fork_at(&revision_frontiers).map_err(loro_test_error)?;
    assert_eq!(body_text(&revision_doc).to_string(), body_text(&reference).to_string());
    assert_eq!(body_text(&revision_doc).to_string(), "\nfirst second");
    assert_eq!(revision_doc.state_frontiers(), reference.state_frontiers());
    assert_eq!(revision_doc.state_vv(), reference.state_vv());
    let revision = package
      .revisions
      .iter()
      .find(|revision| revision.revision_id == revision_id)
      .expect("named revision entry");
    assert_eq!(revision.version_vector, encode_version_vector(&reference.state_vv()));
    Ok(())
  }

  #[test]
  fn sync_revisions_from_loro_snapshots_remote_revision_without_forking() -> io::Result<()> {
    let doc = new_loro_document("Sync revisions").map_err(loro_test_error)?;
    body_text(&doc)
      .insert(1, "shared")
      .map_err(loro_test_error)?;
    doc.commit();
    let mut package = DocumentPackage::from_loro_snapshot(&doc, "Sync revisions")?;

    // Simulate a remote peer that edits and records a revision at its own
    // frontier, then arrives over sync as one imported update.
    let remote = doc.fork();
    body_text(&remote)
      .insert(body_text(&remote).len_unicode(), " remote")
      .map_err(loro_test_error)?;
    remote.commit();
    let remote_frontiers = remote.state_frontiers();
    let remote_revision_id = 77_u128;
    crate::loro_schema::record_revision(
      &remote,
      remote_revision_id,
      encode_frontiers(&remote_frontiers),
      "Remote",
      "Recorded remotely",
      None,
    )
    .map_err(loro_test_error)?;

    let from_frontier = doc.state_frontiers();
    let from_vv = doc.state_vv();
    let update = remote
      .export(ExportMode::updates(&from_vv))
      .map_err(loro_test_error)?;
    doc.import(&update).map_err(loro_test_error)?;
    package.append_update_segment(&from_frontier, &from_vv, &doc.state_frontiers(), &doc.state_vv(), update)?;

    assert_eq!(package.sync_revisions_from_loro(&doc)?, 1);
    let remote_frontier_bytes = encode_frontiers(&remote_frontiers);
    assert!(
      package
        .snapshot_for_frontier(&remote_frontier_bytes)
        .is_some()
    );

    let revision_doc = package.load_revision_loro_doc(remote_revision_id)?;
    let reference = doc.fork_at(&remote_frontiers).map_err(loro_test_error)?;
    assert_eq!(body_text(&revision_doc).to_string(), body_text(&reference).to_string());
    assert_eq!(body_text(&revision_doc).to_string(), "\nshared remote");
    assert_eq!(revision_doc.state_vv(), reference.state_vv());
    let revision = package
      .revisions
      .iter()
      .find(|revision| revision.revision_id == remote_revision_id)
      .expect("synced revision entry");
    assert_eq!(revision.version_vector, encode_version_vector(&reference.state_vv()));
    Ok(())
  }

  #[test]
  fn package_compacts_update_segments_after_threshold() -> io::Result<()> {
    let doc = new_loro_document("Auto compact").map_err(loro_test_error)?;
    let mut package = DocumentPackage::from_loro_snapshot(&doc, "Auto compact")?;
    let initial_revision = package.create_named_revision(&doc, "Blank", "Before edits", None, None)?;

    for text in ["one", " two"] {
      let from_frontier = doc.state_frontiers();
      let from_vv = doc.state_vv();
      body_text(&doc)
        .insert(body_text(&doc).len_unicode(), text)
        .map_err(loro_test_error)?;
      doc.commit();
      let update = doc
        .export(ExportMode::updates(&from_vv))
        .map_err(loro_test_error)?;
      package.append_update_segment(&from_frontier, &from_vv, &doc.state_frontiers(), &doc.state_vv(), update)?;
    }

    // Creating the named revision records it into the Loro `revisions` list and
    // persists that op as its own update segment (Loro-native revisions), so the
    // two body-text inserts bring the total to three segments.
    assert_eq!(package.loro_update_segments.len(), 3);
    let snapshot_id = package.compact_update_segments_if_needed(&doc, 1)?;
    assert!(snapshot_id.is_some());
    assert!(package.loro_update_segments.is_empty());

    let latest_doc = package.load_loro_doc()?;
    assert_eq!(body_text(&latest_doc).to_string(), "\none two");
    let revision_doc = package.load_revision_loro_doc(initial_revision)?;
    assert_eq!(body_text(&revision_doc).to_string(), "\n");
    Ok(())
  }

  #[test]
  fn package_rebuilds_search_units_from_loro_body_flow() -> io::Result<()> {
    let doc = new_loro_document("Search").map_err(loro_test_error)?;
    let text = body_text(&doc);
    text.insert(1, "Alpha\nBeta").map_err(loro_test_error)?;
    doc.commit();

    let mut package = DocumentPackage::from_loro_snapshot(&doc, "Search")?;
    package.rebuild_search_units_from_loro(&doc)?;

    assert_eq!(package.search_units.len(), 2);
    assert_eq!(package.search_units[0].body, "Alpha");
    assert_eq!(package.search_units[1].body, "Beta");
    assert_eq!(
      package.manifest.search_cache_frontier.as_deref(),
      Some(package.manifest.latest_frontier.as_slice())
    );
    assert!(!package.search_units[0].paragraph_start_cursor.is_empty());
    assert!(!package.search_units[0].paragraph_end_cursor.is_empty());
    Ok(())
  }

  #[test]
  fn table_cells_use_column_ids_and_project_by_column_order() -> io::Result<()> {
    let source = crate::document_from_input_blocks(
      crate::flowstate_document_theme(),
      vec![InputBlock::Table(InputTableBlock {
        rows: vec![InputTableRow {
          id: RowId(1),
          cells: vec![
            InputTableCell {
              id: CellId::from_coordinate(RowId(1), ColumnId(1)),
              row_id: RowId(1),
              column_id: ColumnId(1),
              blocks: vec![InputTableCellBlock::Paragraph(InputParagraph {
                style: crate::ParagraphStyle::Normal,
                runs: vec![InputRun {
                  text: "left".to_string(),
                  styles: RunStyles::default(),
                }],
              })],
              row_span: 1,
              col_span: 1,
            },
            InputTableCell {
              id: CellId::from_coordinate(RowId(1), ColumnId(2)),
              row_id: RowId(1),
              column_id: ColumnId(2),
              blocks: vec![InputTableCellBlock::Paragraph(InputParagraph {
                style: crate::ParagraphStyle::Normal,
                runs: vec![InputRun {
                  text: "right".to_string(),
                  styles: RunStyles::default(),
                }],
              })],
              row_span: 1,
              col_span: 1,
            },
          ],
        }],
        columns: vec![
          InputTableColumn {
            id: ColumnId(1),
            width: InputTableColumnWidth::Auto,
          },
          InputTableColumn {
            id: ColumnId(2),
            width: InputTableColumnWidth::Auto,
          },
        ],
        style: InputTableStyle { header_row: false },
      })],
    );
    let doc = document_to_loro(&source, "Table schema")?;
    let table_owner = first_table_owner(&doc)?;
    let table = test_child_map(&table_owner, "table")?;
    let column_ids = test_ordered_ids(&table, "column_order")?;
    assert_eq!(column_ids.len(), 2);

    let cells_by_id = test_child_map(&table, "cells_by_id")?;
    let mut seen_column_ids = Vec::new();
    for cell_id in cells_by_id.keys().map(|key| key.to_string()) {
      let cell = test_child_map(&cells_by_id, &cell_id)?;
      assert!(cell.get("column_index").is_none());
      seen_column_ids.push(test_map_string(&cell, "column_id")?);
    }
    assert!(seen_column_ids.contains(&column_ids[0]));
    assert!(seen_column_ids.contains(&column_ids[1]));

    let column_order = test_child_movable_list(&table, "column_order")?;
    column_order.mov(0, 1).map_err(loro_test_error)?;
    doc.commit();

    let projected = document_from_loro(&doc)?;
    let projected_table = projected
      .blocks
      .iter()
      .find_map(|block| match block {
        Block::Table(table) => Some(table),
        _ => None,
      })
      .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "projected table is missing"))?;
    let cell_texts = projected_table.rows[0]
      .cells
      .iter()
      .map(|cell| match &cell.blocks[0] {
        TableCellBlock::Paragraph(paragraph) => paragraph.text.as_str(),
        TableCellBlock::Table(_) => "",
      })
      .collect::<Vec<_>>();
    assert_eq!(cell_texts, vec!["right", "left"]);
    Ok(())
  }

  #[test]
  fn nested_tables_use_stable_list_refs_and_project_by_anchor_cursor() -> io::Result<()> {
    let nested_table = InputTableBlock {
      // Distinct ids from the outer table: row/column ids are globally unique
      // durable ids, so the inner and outer cells must not collide on a shared
      // coordinate (which would collide their global cell text flows).
      rows: vec![InputTableRow {
        id: RowId(2),
        cells: vec![InputTableCell {
          id: CellId::from_coordinate(RowId(2), ColumnId(2)),
          row_id: RowId(2),
          column_id: ColumnId(2),
          blocks: vec![InputTableCellBlock::Paragraph(InputParagraph {
            style: crate::ParagraphStyle::Normal,
            runs: vec![InputRun {
              text: "inner".to_string(),
              styles: RunStyles::default(),
            }],
          })],
          row_span: 1,
          col_span: 1,
        }],
      }],
      columns: vec![InputTableColumn {
        id: ColumnId(2),
        width: InputTableColumnWidth::Auto,
      }],
      style: InputTableStyle { header_row: false },
    };
    let source = crate::document_from_input_blocks(
      crate::flowstate_document_theme(),
      vec![InputBlock::Table(InputTableBlock {
        rows: vec![InputTableRow {
          id: RowId(1),
          cells: vec![InputTableCell {
            id: CellId::from_coordinate(RowId(1), ColumnId(1)),
            row_id: RowId(1),
            column_id: ColumnId(1),
            blocks: vec![
              InputTableCellBlock::Paragraph(InputParagraph {
                style: crate::ParagraphStyle::Normal,
                runs: vec![InputRun {
                  text: "outer".to_string(),
                  styles: RunStyles::default(),
                }],
              }),
              InputTableCellBlock::Table(nested_table),
            ],
            row_span: 1,
            col_span: 1,
          }],
        }],
        columns: vec![InputTableColumn {
          id: ColumnId(1),
          width: InputTableColumnWidth::Auto,
        }],
        style: InputTableStyle { header_row: false },
      })],
    );
    let doc = document_to_loro(&source, "Nested table")?;
    let table = test_child_map(&first_table_owner(&doc)?, "table")?;
    let cell = first_cell_map(&table)?;
    assert_eq!(test_ordered_ids(&cell, "nested_table_ids")?.len(), 1);
    assert!(test_child_map(&cell, "nested_tables_by_id").is_ok());
    assert!(!cell.keys().any(|key| key.starts_with("nested_table.")));

    let projected = document_from_loro(&doc)?;
    let projected_table = projected
      .blocks
      .iter()
      .find_map(|block| match block {
        Block::Table(table) => Some(table),
        _ => None,
      })
      .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "projected table is missing"))?;
    let blocks = &projected_table.rows[0].cells[0].blocks;
    assert!(matches!(&blocks[0], TableCellBlock::Paragraph(paragraph) if paragraph.text == "outer"));
    assert!(
      matches!(&blocks[1], TableCellBlock::Table(table) if matches!(&table.rows[0].cells[0].blocks[0], TableCellBlock::Paragraph(paragraph) if paragraph.text == "inner"))
    );
    Ok(())
  }

  #[test]
  fn package_search_units_come_from_projection_objects_and_tables() -> io::Result<()> {
    let source = crate::document_from_input_blocks(
      crate::flowstate_document_theme(),
      vec![
        InputBlock::Paragraph(InputParagraph {
          style: crate::ParagraphStyle::Normal,
          runs: vec![InputRun {
            text: "Body".to_string(),
            styles: RunStyles::default(),
          }],
        }),
        InputBlock::Image(InputImageBlock {
          asset_id: AssetId(7),
          alt_text: "diagram alt".to_string(),
          caption: Some(InputParagraph {
            style: crate::ParagraphStyle::Normal,
            runs: vec![InputRun {
              text: "caption text".to_string(),
              styles: RunStyles::default(),
            }],
          }),
          sizing: InputImageSizing::FitWidth,
          alignment: InputBlockAlignment::Left,
          external_url: None,
        }),
        InputBlock::Equation(InputEquationBlock {
          source: "x^2".to_string(),
          syntax: InputEquationSyntax::Latex,
          display: InputEquationDisplay::Display,
        }),
        InputBlock::Table(InputTableBlock {
          rows: vec![InputTableRow {
            id: RowId(1),
            cells: vec![InputTableCell {
              id: CellId::from_coordinate(RowId(1), ColumnId(1)),
              row_id: RowId(1),
              column_id: ColumnId(1),
              blocks: vec![InputTableCellBlock::Paragraph(InputParagraph {
                style: crate::ParagraphStyle::Normal,
                runs: vec![InputRun {
                  text: "cell text".to_string(),
                  styles: RunStyles::default(),
                }],
              })],
              row_span: 1,
              col_span: 1,
            }],
          }],
          columns: vec![InputTableColumn {
            id: ColumnId(1),
            width: InputTableColumnWidth::Auto,
          }],
          style: InputTableStyle { header_row: false },
        }),
      ],
    );
    let doc = document_to_loro(&source, "Search projection")?;
    let image = first_block_owner_by_kind(&doc, "image")?;
    let caption_flow_id = test_map_string(&image, "caption_flow_id")?;
    let root = doc.get_map(crate::ROOT);
    let flows = test_child_map(&root, crate::FLOWS_BY_ID)?;
    let caption_flow = test_child_map(&flows, &caption_flow_id)?;
    let caption_text = test_child_text(&caption_flow, crate::FLOW_TEXT_KEY)?;
    caption_text
      .insert(1, "caption text")
      .map_err(loro_test_error)?;
    doc.commit();
    let package = DocumentPackage::from_loro_snapshot(&doc, "Search projection")?;
    let units = package.current_search_units();

    assert!(
      units
        .iter()
        .any(|unit| unit.unit_kind == "paragraph" && unit.body == "Body")
    );
    assert!(
      units
        .iter()
        .any(|unit| unit.unit_kind == "image_alt" && unit.body == "diagram alt")
    );
    assert!(
      units
        .iter()
        .any(|unit| unit.unit_kind == "image_caption" && unit.body == "caption text")
    );
    assert!(
      units
        .iter()
        .any(|unit| unit.unit_kind == "equation" && unit.body == "x^2")
    );
    assert!(
      units
        .iter()
        .any(|unit| unit.unit_kind == "table_cell" && unit.body == "cell text")
    );
    assert!(
      units
        .iter()
        .all(|unit| !unit.body.contains(crate::OBJECT_REPLACEMENT))
    );
    let body_unit = units
      .iter()
      .find(|unit| unit.unit_kind == "paragraph" && unit.body == "Body")
      .expect("body paragraph search unit should exist");
    assert!(!body_unit.paragraph_start_cursor.is_empty());
    assert!(!body_unit.paragraph_end_cursor.is_empty());
    assert_eq!(
      package.manifest.search_cache_frontier.as_deref(),
      Some(package.manifest.latest_frontier.as_slice())
    );
    Ok(())
  }

  #[test]
  fn public_db8_roundtrips_structured_loro_objects_and_assets() -> io::Result<()> {
    let asset_id = AssetId(42);
    let asset_bytes = b"not really a png".to_vec();
    let mut source = crate::document_from_input_blocks(
      crate::flowstate_document_theme(),
      vec![
        InputBlock::Paragraph(InputParagraph {
          style: crate::ParagraphStyle::Normal,
          runs: vec![InputRun {
            text: "before".to_string(),
            styles: RunStyles::default(),
          }],
        }),
        InputBlock::Image(InputImageBlock {
          asset_id,
          alt_text: "diagram alt".to_string(),
          caption: Some(InputParagraph {
            style: crate::ParagraphStyle::Custom(1),
            runs: vec![InputRun {
              text: "caption".to_string(),
              styles: RunStyles::default(),
            }],
          }),
          sizing: InputImageSizing::Fixed {
            width_px: 320,
            height_px: Some(180),
          },
          alignment: InputBlockAlignment::Center,
          external_url: None,
        }),
        InputBlock::Equation(InputEquationBlock {
          source: "x^2".to_string(),
          syntax: InputEquationSyntax::Latex,
          display: InputEquationDisplay::InlineLikeParagraph,
        }),
        InputBlock::Table(InputTableBlock {
          rows: vec![InputTableRow {
            id: RowId(1),
            cells: vec![InputTableCell {
              id: CellId::from_coordinate(RowId(1), ColumnId(1)),
              row_id: RowId(1),
              column_id: ColumnId(1),
              blocks: vec![InputTableCellBlock::Paragraph(InputParagraph {
                style: crate::ParagraphStyle::Normal,
                runs: vec![InputRun {
                  text: "cell".to_string(),
                  styles: RunStyles::default(),
                }],
              })],
              row_span: 1,
              col_span: 1,
            }],
          }],
          columns: vec![InputTableColumn {
            id: ColumnId(1),
            width: InputTableColumnWidth::FixedPx(144),
          }],
          style: InputTableStyle { header_row: true },
        }),
      ],
    );
    source.assets.assets.insert(
      asset_id,
      AssetRecord {
        id: asset_id,
        mime_type: "image/png".into(),
        original_name: Some("diagram.png".into()),
        content_hash: AssetRecord::stable_content_hash(&asset_bytes),
        bytes: Arc::new(asset_bytes.clone()),
      },
    );

    let doc = document_to_loro(&source, "Structured").map_err(loro_test_error)?;
    let bytes =
      DocumentPackage::from_loro_snapshot_with_assets(&doc, "Structured", crate::loro_import::assets_from_document(&source))?.to_bytes()?;
    let loaded = read_db8_bytes(&bytes)?;
    assert_eq!(crate::paragraph_text(&loaded, 0), "before");
    assert_eq!(
      loaded
        .assets
        .assets
        .get(&asset_id)
        .map(|asset| asset.bytes.as_ref().clone()),
      Some(asset_bytes)
    );

    assert!(matches!(
      &loaded.blocks[1],
      Block::Image(image)
        if image.asset_id == asset_id
          && image.alt_text.as_ref() == "diagram alt"
          && image.caption.as_ref().is_some_and(|caption| caption.style == crate::ParagraphStyle::Custom(1))
    ));
    assert!(matches!(
      &loaded.blocks[2],
      Block::Equation(equation)
        if equation.source.as_ref() == "x^2" && equation.display == crate::EquationDisplay::InlineLikeParagraph
    ));
    assert!(matches!(
      &loaded.blocks[3],
      Block::Table(table)
        if table.style.header_row
          && matches!(
            table.columns.as_slice(),
            [crate::TableColumn {
              width: crate::TableColumnWidth::FixedPx(144),
              ..
            }]
          )
          && matches!(&table.rows[0].cells[0].blocks[0], TableCellBlock::Paragraph(paragraph) if paragraph.text == "cell")
    ));
    Ok(())
  }

  #[test]
  fn package_serializes_empty_schema_migration_log() -> io::Result<()> {
    let doc = new_loro_document("Migrations").map_err(loro_test_error)?;
    let bytes = DocumentPackage::from_loro_snapshot(&doc, "Migrations")?.to_bytes()?;
    let package = DocumentPackage::from_bytes(&bytes)?;
    assert!(package.manifest.schema_migrations.is_empty());
    Ok(())
  }

  #[test]
  fn package_rejects_inconsistent_schema_migration_record() -> io::Result<()> {
    let doc = new_loro_document("Migrations").map_err(loro_test_error)?;
    let mut package = DocumentPackage::from_loro_snapshot(&doc, "Migrations")?;
    package
      .manifest
      .schema_migrations
      .push(SchemaMigrationRecord {
        id: 1,
        from_version: 2,
        to_version: 1,
        applied_at_unix_secs: 0,
        description: "backwards".to_string(),
      });

    let error = package
      .validate()
      .expect_err("a backwards migration record must be rejected");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    Ok(())
  }

  #[test]
  fn package_integrity_index_covers_durable_chunks() -> io::Result<()> {
    let doc = new_loro_document("Integrity").map_err(loro_test_error)?;
    let bytes = b"asset bytes".to_vec();
    let asset = AssetChunk {
      asset_id: 7,
      content_hash: blake3_hash(&bytes),
      mime_type: "application/octet-stream".to_string(),
      byte_length: bytes.len() as u64,
      bytes,
      metadata: Vec::new(),
    };
    let package = DocumentPackage::from_loro_snapshot_with_assets(&doc, "Integrity", vec![asset])?;

    assert!(
      package
        .integrity_index
        .iter()
        .any(|entry| entry.chunk_kind == CHUNK_ASSET && entry.id == 7)
    );
    assert_eq!(
      package
        .integrity_index
        .iter()
        .filter(|entry| entry.chunk_kind == CHUNK_LORO_SNAPSHOT)
        .count(),
      package.loro_snapshots.len(),
    );

    let roundtrip = DocumentPackage::from_bytes(&package.to_bytes()?)?;
    assert_eq!(roundtrip.integrity_index.len(), package.integrity_index.len());
    Ok(())
  }

  #[test]
  fn package_rejects_tampered_integrity_index() -> io::Result<()> {
    let doc = new_loro_document("Integrity").map_err(loro_test_error)?;
    let mut package = DocumentPackage::from_loro_snapshot(&doc, "Integrity")?;
    let entry = package
      .integrity_index
      .first_mut()
      .expect("at least one integrity entry");
    entry.checksum = [0; 32];

    let error = package
      .validate()
      .expect_err("a tampered integrity entry must be rejected");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    Ok(())
  }

  fn first_table_owner(doc: &LoroDoc) -> io::Result<LoroMap> {
    first_block_owner_by_kind(doc, "table")
  }

  fn first_block_owner_by_kind(doc: &LoroDoc, kind: &str) -> io::Result<LoroMap> {
    let root = doc.get_map(crate::ROOT);
    let blocks = test_child_map(&root, crate::BLOCKS_BY_ID)?;
    for block_id in blocks.keys().map(|key| key.to_string()) {
      let block = test_child_map(&blocks, &block_id)?;
      if test_map_string_opt(&block, "kind").as_deref() == Some(kind) {
        return Ok(block);
      }
    }
    Err(io::Error::new(io::ErrorKind::InvalidData, format!("{kind} block is missing")))
  }

  fn first_cell_map(table: &LoroMap) -> io::Result<LoroMap> {
    let cells_by_id = test_child_map(table, "cells_by_id")?;
    let cell_id = cells_by_id
      .keys()
      .next()
      .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "table cell is missing"))?
      .to_string();
    test_child_map(&cells_by_id, &cell_id)
  }

  fn test_child_map(parent: &LoroMap, key: &str) -> io::Result<LoroMap> {
    match parent.get(key) {
      Some(ValueOrContainer::Container(container)) => container
        .into_map()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, format!("`{key}` is not a map"))),
      _ => Err(io::Error::new(io::ErrorKind::InvalidData, format!("missing map `{key}`"))),
    }
  }

  fn test_child_movable_list(parent: &LoroMap, key: &str) -> io::Result<loro::LoroMovableList> {
    match parent.get(key) {
      Some(ValueOrContainer::Container(Container::MovableList(list))) => Ok(list),
      _ => Err(io::Error::new(io::ErrorKind::InvalidData, format!("missing movable list `{key}`"))),
    }
  }

  fn test_child_text(parent: &LoroMap, key: &str) -> io::Result<loro::LoroText> {
    match parent.get(key) {
      Some(ValueOrContainer::Container(container)) => container
        .into_text()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, format!("`{key}` is not text"))),
      _ => Err(io::Error::new(io::ErrorKind::InvalidData, format!("missing text `{key}`"))),
    }
  }

  fn test_ordered_ids(parent: &LoroMap, key: &str) -> io::Result<Vec<String>> {
    Ok(
      test_child_movable_list(parent, key)?
        .to_vec()
        .into_iter()
        .filter_map(|value| match value {
          LoroValue::String(value) => Some(value.to_string()),
          _ => None,
        })
        .collect(),
    )
  }

  fn test_map_string(map: &LoroMap, key: &str) -> io::Result<String> {
    test_map_string_opt(map, key).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("missing string `{key}`")))
  }

  fn test_map_string_opt(map: &LoroMap, key: &str) -> Option<String> {
    map.get(key).and_then(|value| match value {
      ValueOrContainer::Value(LoroValue::String(value)) => Some(value.to_string()),
      _ => None,
    })
  }

  fn loro_test_error(error: impl std::error::Error + Send + Sync + 'static) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
  }

  /// §act-four M2: the preview header materializes the leading blocks and
  /// matches the full document there; and — riding the journal delta — it stays
  /// fresh for an edited-but-unflushed document, exactly the "preview without a
  /// solid full cache" case. Read is O(header): only the manifest + header
  /// chunk decode.
  #[test]
  fn preview_header_matches_leading_blocks_and_stays_fresh_across_edits() -> io::Result<()> {
    use crate::{DocumentTheme, ParagraphStyle, document_from_input, import_document_projection};

    let paras: Vec<InputParagraph> = (0..60)
      .map(|i| InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![InputRun {
          text: format!("paragraph {i:03} of the document body"),
          styles: RunStyles::default(),
        }],
      })
      .collect();
    let projection = document_from_input(DocumentTheme::default(), paras);
    let imported = import_document_projection(projection, "Header Test").map_err(loro_test_error)?;
    let doc = imported.doc;
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("header.db8");
    let mut package = DocumentPackage::from_loro_snapshot(&doc, "Header Test")?;
    package.write(&path)?;

    // Checkpoint header == leading blocks of the full materialization.
    let header = DocumentPackage::read_preview_header(&path)?.expect("fresh header at checkpoint");
    assert!(
      !header.paragraphs.is_empty() && header.paragraphs.len() <= PREVIEW_HEADER_BLOCKS,
      "header trims to the leading blocks"
    );
    let full = document_from_loro(&doc).map_err(loro_test_error)?;
    for ix in 0..header.paragraphs.len() {
      assert_eq!(
        crate::paragraph_text(&header, ix),
        crate::paragraph_text(&full, ix),
        "header paragraph {ix} matches the full doc"
      );
    }

    // Edit the document, persist as a journal delta (no full checkpoint). The
    // full projection cache is nulled — but the header rides the delta and
    // stays fresh.
    let text = body_text(&doc);
    let from_frontier = doc.state_frontiers();
    let from_vv = doc.state_vv();
    text.insert(1, "EDIT ").map_err(loro_test_error)?;
    doc.commit();
    let update = doc
      .export(loro::ExportMode::updates(&from_vv))
      .map_err(loro_test_error)?;
    package.append_update_segment(&from_frontier, &from_vv, &doc.state_frontiers(), &doc.state_vv(), update)?;
    assert!(
      package.manifest.projection_cache_frontier.is_none(),
      "an edit nulls the full projection cache"
    );
    let edited_projection = document_from_loro(&doc).map_err(loro_test_error)?;
    package.refresh_preview_header(&edited_projection)?;
    package.append_latest_update_to_path(&path)?;

    // The full cache is absent, but the header preview is fresh and correct.
    assert!(
      DocumentPackage::read_cached_projection(&path)?.is_none(),
      "no fresh full cache after the edit"
    );
    let header2 = DocumentPackage::read_preview_header(&path)?.expect("header fresh after edit via the journal delta");
    assert_eq!(header2.frontier, package.manifest.latest_frontier, "header is at the latest frontier");
    assert_eq!(
      crate::paragraph_text(&header2, 0),
      "EDIT paragraph 000 of the document body",
      "header reflects the edit"
    );
    Ok(())
  }

  /// 2026-07-07 .db8 first-edit-freeze regressions: (a) appending a segment
  /// must be O(delta) — it must not re-validate the WHOLE package (whose full
  /// validation previously imported the entire Loro snapshot per keystroke);
  /// (b) reading a stacked journal must decode only the LAST generation.
  #[test]
  fn append_update_segment_validates_only_the_delta() -> io::Result<()> {
    let doc = new_loro_document("Delta validate").map_err(loro_test_error)?;
    let mut package = DocumentPackage::from_loro_snapshot(&doc, "Delta validate")?;
    let from_frontier = doc.state_frontiers();
    let from_vv = doc.state_vv();
    body_text(&doc).insert(1, "x").map_err(loro_test_error)?;
    doc.commit();
    let update = doc
      .export(ExportMode::updates(&from_vv))
      .map_err(loro_test_error)?;
    package.append_update_segment(&from_frontier, &from_vv, &doc.state_frontiers(), &doc.state_vv(), update)?;

    // Corrupt HISTORY (the stored first segment's checksum): a delta-scoped
    // append must still succeed, while a full validation must reject it.
    package.loro_update_segments[0].checksum = [0_u8; 32];
    let from_frontier = doc.state_frontiers();
    let from_vv = doc.state_vv();
    body_text(&doc).insert(1, "y").map_err(loro_test_error)?;
    doc.commit();
    let update = doc
      .export(ExportMode::updates(&from_vv))
      .map_err(loro_test_error)?;
    package
      .append_update_segment(&from_frontier, &from_vv, &doc.state_frontiers(), &doc.state_vv(), update)
      .expect("append validates only the delta, not all history");
    assert!(package.validate().is_err(), "full validation still catches the historical corruption");
    Ok(())
  }

  #[test]
  fn journal_read_decodes_only_the_latest_generation() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("generations.db8");
    let doc = new_loro_document("Generations").map_err(loro_test_error)?;
    let mut package = DocumentPackage::from_loro_snapshot(&doc, "Generations")?;
    package.write(&path)?;
    // Stack several full generations by rewriting through the journal append
    // path (each `write` under the compaction threshold appends a generation).
    for round in 0..3 {
      let from_frontier = doc.state_frontiers();
      let from_vv = doc.state_vv();
      body_text(&doc)
        .insert(1, &format!("g{round}"))
        .map_err(loro_test_error)?;
      doc.commit();
      let update = doc
        .export(ExportMode::updates(&from_vv))
        .map_err(loro_test_error)?;
      package.append_update_segment(&from_frontier, &from_vv, &doc.state_frontiers(), &doc.state_vv(), update)?;
      package.write(&path)?;
    }
    let bytes = fs::read(&path)?;
    let payloads = committed_journal_payloads(&bytes)?;
    let generations = payloads
      .iter()
      .filter(|payload| payload.starts_with(PACKAGE_MAGIC))
      .count();
    assert!(generations >= 2, "test setup must stack multiple generations, got {generations}");
    let read_back = DocumentPackage::read(&path)?;
    assert_eq!(read_back.manifest.latest_frontier, package.manifest.latest_frontier);
    assert_eq!(read_back.loro_update_segments.len(), package.loro_update_segments.len());
    Ok(())
  }

  #[test]
  fn shallow_snapshot_chunk_round_trips_and_decodes_shallow() -> io::Result<()> {
    let doc = new_loro_document("Shallow").map_err(loro_test_error)?;
    let text = body_text(&doc);
    text
      .insert(text.len_unicode(), "early history")
      .map_err(loro_test_error)?;
    doc.commit();
    let mid_frontier = doc.state_frontiers();
    text
      .insert(text.len_unicode(), " and the recent window")
      .map_err(loro_test_error)?;
    doc.commit();

    let mut package = DocumentPackage::from_loro_snapshot(&doc, "Shallow")?;
    package.record_shallow_snapshot(&doc, &mid_frontier)?;
    let bytes = package.to_bytes()?;

    let reloaded = DocumentPackage::from_bytes(&bytes)?;
    let shallow = reloaded
      .latest_shallow_snapshot()
      .expect("shallow chunk survives the round trip");
    assert_eq!(reloaded.manifest.shallow_snapshot_frontier.as_deref(), Some(shallow.frontier.as_slice()));
    // The accelerator never displaces the full snapshot (invariant S1).
    assert!(!reloaded.loro_snapshots.is_empty());

    let probe = LoroDoc::new();
    probe.import(&shallow.bytes).map_err(loro_test_error)?;
    assert!(probe.is_shallow(), "decoded chunk must be a shallow doc");
    assert_eq!(body_text(&probe).to_string(), body_text(&doc).to_string());
    assert_eq!(
      encode_frontiers(&probe.shallow_since_frontiers()),
      shallow.root_frontier,
      "recorded root must match the decoded doc's shallow root"
    );
    assert_eq!(encode_frontiers(&probe.oplog_frontiers()), shallow.frontier);
    Ok(())
  }

  #[test]
  fn clear_shallow_snapshot_round_trips_clean() -> io::Result<()> {
    let doc = new_loro_document("Shallow clear").map_err(loro_test_error)?;
    let text = body_text(&doc);
    text
      .insert(text.len_unicode(), "content")
      .map_err(loro_test_error)?;
    doc.commit();
    let root = doc.state_frontiers();

    let mut package = DocumentPackage::from_loro_snapshot(&doc, "Shallow clear")?;
    package.record_shallow_snapshot(&doc, &root)?;
    package.clear_shallow_snapshot();
    let reloaded = DocumentPackage::from_bytes(&package.to_bytes()?)?;
    assert!(reloaded.latest_shallow_snapshot().is_none());
    assert!(reloaded.manifest.shallow_snapshot_frontier.is_none());
    Ok(())
  }

  #[test]
  fn shallow_stamp_without_chunk_fails_validate() -> io::Result<()> {
    let doc = new_loro_document("Shallow stamp").map_err(loro_test_error)?;
    let mut package = DocumentPackage::from_loro_snapshot(&doc, "Shallow stamp")?;
    package.manifest.shallow_snapshot_frontier = Some(package.manifest.latest_frontier.clone());
    let error = package
      .validate()
      .expect_err("stamp without a chunk must fail");
    assert!(error.to_string().contains("shallow snapshot"));
    Ok(())
  }

  #[test]
  fn shallow_load_equals_full_load_with_trailing_segments() -> io::Result<()> {
    let doc = new_loro_document("Shallow equivalence").map_err(loro_test_error)?;
    let text = body_text(&doc);
    text
      .insert(text.len_unicode(), "history before the root")
      .map_err(loro_test_error)?;
    doc.commit();
    let root = doc.state_frontiers();
    text
      .insert(text.len_unicode(), " | window before checkpoint")
      .map_err(loro_test_error)?;
    doc.commit();

    let mut package = DocumentPackage::from_loro_snapshot(&doc, "Shallow equivalence")?;
    package.compact_to_snapshot(&doc)?;
    package.record_shallow_snapshot(&doc, &root)?;

    // Post-checkpoint segment appended AFTER the shallow chunk was minted —
    // the shallow load must replay it on top of the shallow state.
    let from_frontier = doc.state_frontiers();
    let from_vv = doc.state_vv();
    text
      .insert(text.len_unicode(), " | post-checkpoint edits")
      .map_err(loro_test_error)?;
    doc.commit();
    let update = doc
      .export(ExportMode::updates(&from_vv))
      .map_err(loro_test_error)?;
    package.append_update_segment(&from_frontier, &from_vv, &doc.state_frontiers(), &doc.state_vv(), update)?;

    let reloaded = DocumentPackage::from_bytes(&package.to_bytes()?)?;
    let shallow_doc = reloaded
      .load_loro_doc_shallow()?
      .expect("shallow chunk + trailing segment must reconstruct the tip");
    assert!(shallow_doc.is_shallow());
    let full_doc = reloaded.load_loro_doc()?;
    assert_eq!(body_text(&shallow_doc).to_string(), body_text(&full_doc).to_string());
    assert_eq!(
      encode_frontiers(&shallow_doc.oplog_frontiers()),
      encode_frontiers(&full_doc.oplog_frontiers())
    );
    let shallow_projection = crate::document_from_loro(&shallow_doc).map_err(loro_test_error)?;
    let full_projection = crate::document_from_loro(&full_doc).map_err(loro_test_error)?;
    assert_eq!(shallow_projection.ids.paragraph_ids, full_projection.ids.paragraph_ids);
    assert_eq!(shallow_projection.paragraphs.len(), full_projection.paragraphs.len());

    // A stale stamp (frontier no longer reconstructing the tip) must demote
    // to the full path, not open a wrong version.
    let mut stale = reloaded.clone();
    stale.manifest.latest_frontier = vec![9, 9, 9];
    assert!(stale.load_loro_doc_shallow()?.is_none());
    Ok(())
  }

  #[test]
  fn compaction_refuses_a_shallow_live_doc() -> io::Result<()> {
    let doc = new_loro_document("Shallow compact guard").map_err(loro_test_error)?;
    let text = body_text(&doc);
    text
      .insert(text.len_unicode(), "history")
      .map_err(loro_test_error)?;
    doc.commit();
    let root = doc.state_frontiers();
    text
      .insert(text.len_unicode(), " tip")
      .map_err(loro_test_error)?;
    doc.commit();
    let mut package = DocumentPackage::from_loro_snapshot(&doc, "Shallow compact guard")?;
    package.record_shallow_snapshot(&doc, &root)?;
    let shallow_doc = package
      .load_loro_doc_shallow()?
      .expect("shallow chunk reconstructs the tip");

    let error = package
      .compact_to_snapshot(&shallow_doc)
      .expect_err("compacting a shallow live doc must refuse (S1)");
    assert!(error.to_string().contains("shallow"));
    // The inline backstop silently skips instead of erroring.
    assert!(
      package
        .compact_update_segments_if_needed(&shallow_doc, 1)?
        .is_none()
    );
    // The package is untouched: full snapshot still present, still loadable.
    package.load_loro_doc()?;
    Ok(())
  }

  #[test]
  fn shallow_session_checkpoint_preserves_full_history() -> io::Result<()> {
    let doc = new_loro_document("Shallow checkpoint").map_err(loro_test_error)?;
    let text = body_text(&doc);
    text
      .insert(text.len_unicode(), "ancient history")
      .map_err(loro_test_error)?;
    doc.commit();
    let ancient_frontier = doc.state_frontiers();
    text
      .insert(text.len_unicode(), " | window")
      .map_err(loro_test_error)?;
    doc.commit();

    let mut package = DocumentPackage::from_loro_snapshot(&doc, "Shallow checkpoint")?;
    package.compact_to_snapshot(&doc)?;
    package.record_shallow_snapshot(&doc, &ancient_frontier)?;

    // A shallow SESSION: open from the shallow chunk, edit, checkpoint.
    let live = package
      .load_loro_doc_shallow()?
      .expect("shallow chunk reconstructs the tip");
    assert!(live.is_shallow());
    let from_frontier = live.state_frontiers();
    let from_vv = live.state_vv();
    let live_text = body_text(&live);
    live_text
      .insert(live_text.len_unicode(), " | shallow session edit")
      .map_err(loro_test_error)?;
    live.commit();
    let update = live
      .export(ExportMode::updates(&from_vv))
      .map_err(loro_test_error)?;
    package.append_update_segment(&from_frontier, &from_vv, &live.state_frontiers(), &live.state_vv(), update)?;

    package.compact_to_snapshot_any(&live)?;

    let reloaded = DocumentPackage::from_bytes(&package.to_bytes()?)?;
    // The full chunk carries COMPLETE history: not shallow, and the ancient
    // pre-root frontier is still reachable.
    let full = reloaded.load_loro_doc()?;
    assert!(!full.is_shallow(), "compaction from a shallow session must persist FULL history");
    assert_eq!(body_text(&full).to_string(), body_text(&live).to_string());
    full.checkout(&ancient_frontier).map_err(loro_test_error)?;
    full.checkout_to_latest();
    // The shallow accelerator was re-minted at the new checkpoint tip.
    let shallow = reloaded
      .latest_shallow_snapshot()
      .expect("checkpoint re-mints the shallow accelerator");
    assert_eq!(shallow.frontier, reloaded.manifest.latest_frontier);
    let next_live = reloaded
      .load_loro_doc_shallow()?
      .expect("next open takes the shallow path again");
    assert_eq!(body_text(&next_live).to_string(), body_text(&live).to_string());
    Ok(())
  }

  #[test]
  fn pre_root_revision_loads_from_a_shallow_session_package() -> io::Result<()> {
    // G3: time-travel below the shallow root is served from the package's
    // retained full history, never the live doc.
    let doc = new_loro_document("Shallow revision").map_err(loro_test_error)?;
    let text = body_text(&doc);
    text
      .insert(text.len_unicode(), "ancient state")
      .map_err(loro_test_error)?;
    doc.commit();
    let mut package = DocumentPackage::from_loro_snapshot(&doc, "Shallow revision")?;
    let (ancient_revision, _) = package.compact_to_named_snapshot(&doc, "Ancient", "pre-root", None, None)?;

    text
      .insert(text.len_unicode(), " | later work")
      .map_err(loro_test_error)?;
    doc.commit();
    let root = doc.state_frontiers();
    text
      .insert(text.len_unicode(), " | window")
      .map_err(loro_test_error)?;
    doc.commit();
    let from_frontier = package_tip_frontiers(&package)?;
    let from_vv = package_tip_vv(&package)?;
    let update = doc
      .export(ExportMode::updates(&from_vv))
      .map_err(loro_test_error)?;
    package.append_update_segment(&from_frontier, &from_vv, &doc.state_frontiers(), &doc.state_vv(), update)?;
    package.record_shallow_snapshot(&doc, &root)?;

    // Shallow session checkpoints; the ancient revision must still load.
    let live = package.load_loro_doc_shallow()?.expect("shallow session");
    package.compact_to_snapshot_any(&live)?;
    let revision_doc = package.load_revision_loro_doc(ancient_revision)?;
    assert_eq!(body_text(&revision_doc).to_string(), "\nancient state");
    Ok(())
  }

  fn package_tip_frontiers(package: &DocumentPackage) -> io::Result<Frontiers> {
    decode_frontiers(&package.manifest.latest_frontier)
  }

  fn package_tip_vv(package: &DocumentPackage) -> io::Result<VersionVector> {
    VersionVector::decode(&package.manifest.latest_version_vector).map_err(loro_test_error)
  }

  #[test]
  fn deferred_snapshot_hash_still_catches_corruption_at_decode() -> io::Result<()> {
    let doc = new_loro_document("Deferred hash").map_err(loro_test_error)?;
    let text = body_text(&doc);
    text
      .insert(text.len_unicode(), "content to corrupt")
      .map_err(loro_test_error)?;
    doc.commit();
    let package = DocumentPackage::from_loro_snapshot(&doc, "Deferred hash")?;
    let mut bytes = package.to_bytes()?;

    // Flip one byte INSIDE the latest snapshot chunk's encoded payload. The
    // read defers full-snapshot hashing, so from_bytes must still succeed...
    let payload = encode_chunk(package.latest_snapshot().expect("snapshot"), "Loro snapshot")?;
    let payload_at = bytes
      .windows(payload.len())
      .position(|window| window == payload)
      .expect("latest snapshot payload present in the package bytes");
    bytes[payload_at + payload.len() / 2] ^= 0xFF;
    let reloaded = DocumentPackage::from_bytes(&bytes)?;
    // ...but every consumer of those bytes must catch it.
    let load_error = reloaded
      .load_loro_doc()
      .expect_err("corrupt snapshot must fail at decode");
    assert!(load_error.to_string().contains("checksum"), "{load_error}");
    let write_error = reloaded
      .to_bytes()
      .expect_err("corrupt snapshot must never be re-written as valid");
    assert!(write_error.to_string().contains("checksum"), "{write_error}");
    Ok(())
  }

  #[test]
  fn shallow_chunk_survives_journal_tail_repair() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("shallow-tail.db8");
    let doc = new_loro_document("Shallow tail").map_err(loro_test_error)?;
    let text = body_text(&doc);
    text
      .insert(text.len_unicode(), "durable")
      .map_err(loro_test_error)?;
    doc.commit();
    let root = doc.state_frontiers();
    let mut package = DocumentPackage::from_loro_snapshot(&doc, "Shallow tail")?;
    package.record_shallow_snapshot(&doc, &root)?;
    package.write(&path)?;
    let committed_len = fs::metadata(&path)?.len();

    let mut file = OpenOptions::new().append(true).open(&path)?;
    std::io::Write::write_all(&mut file, b"torn shallow tail")?;
    drop(file);

    let repaired = DocumentPackage::read(&path)?;
    repaired.validate()?;
    assert!(repaired.latest_shallow_snapshot().is_some());
    assert_eq!(fs::metadata(&path)?.len(), committed_len);
    Ok(())
  }
}
