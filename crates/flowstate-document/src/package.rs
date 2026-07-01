use std::{
  collections::HashSet,
  fs::{self, OpenOptions},
  io::{self, Cursor, Read as _, Seek as _, SeekFrom, Write as _},
  path::Path,
};

use loro::{Container, ExportMode, Frontiers, LoroDoc, LoroValue, ValueOrContainer, VersionVector};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const LORO_PACKAGE_FORMAT_VERSION: u32 = 1;
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
}

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
  pub created_at_unix_secs: i64,
  pub modified_at_unix_secs: i64,
  /// §27 schema migration log. Empty at schema v1 (no migrations exist yet).
  /// Future schema bumps append a [`SchemaMigrationRecord`] here, and any Loro
  /// document mutation a migration performs is an ordinary Loro change committed
  /// with the `"migration"` origin. Older packages without this field decode to
  /// an empty vector.
  #[serde(default)]
  pub schema_migrations: Vec<SchemaMigrationRecord>,
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
  pub bytes: Vec<u8>,
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
  pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
enum PackageJournalDelta {
  Update {
    manifest: DocumentPackageManifest,
    segment: LoroUpdateSegmentChunk,
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
    let revision_doc = doc.fork_at(&revision_frontiers).map_err(loro_io_error)?;
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
        created_at_unix_secs: now,
        modified_at_unix_secs: now,
        schema_migrations: Vec::new(),
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
          version_vector: encode_version_vector(&revision_doc.state_vv()),
          bytes: revision_doc
            .export(ExportMode::Snapshot)
            .map_err(loro_io_error)?,
          created_at_unix_secs: now,
        },
      ],
      loro_update_segments: Vec::new(),
      assets,
      revisions: vec![PackageRevision {
        revision_id,
        frontier: encode_frontiers(&revision_frontiers),
        version_vector: encode_version_vector(&revision_doc.state_vv()),
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
    }
    .with_manifest_indexes()?;
    package.rebuild_projection_cache_from_loro(doc)?;
    package.rebuild_search_units_from_loro(doc)?;
    Ok(package)
  }

  pub fn load_loro_doc(&self) -> io::Result<LoroDoc> {
    self.validate()?;
    self.load_loro_doc_unvalidated()
  }

  fn load_loro_doc_unvalidated(&self) -> io::Result<LoroDoc> {
    let snapshot = self
      .latest_snapshot()
      .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Flowstate package has no latest Loro snapshot"))?;
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
    if manifest.package_format_version != LORO_PACKAGE_FORMAT_VERSION || manifest.loro_schema_version != LORO_SCHEMA_VERSION {
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
    let mut next = self.clone();
    next.loro_update_segments.push(LoroUpdateSegmentChunk {
      segment_id,
      from_frontier: encode_frontiers(from_frontier),
      from_version_vector: encode_version_vector(from_version_vector),
      to_frontier: encode_frontiers(to_frontier),
      to_version_vector: encode_version_vector(to_version_vector),
      bytes,
      checksum,
      created_at_unix_secs: now,
    });
    next.manifest.latest_frontier = encode_frontiers(to_frontier);
    next.manifest.latest_version_vector = encode_version_vector(to_version_vector);
    next.manifest.projection_cache_frontier = None;
    next.projection_caches.clear();
    next.manifest.search_cache_frontier = None;
    next.search_units.clear();
    next.manifest.modified_at_unix_secs = now;
    next.refresh_manifest_indexes();
    next.validate()?;
    *self = next;
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
    let revision_doc = doc.fork_at(frontiers).map_err(loro_io_error)?;
    let frontier = encode_frontiers(frontiers);
    let version_vector = encode_version_vector(&revision_doc.state_vv());
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
    if self.snapshot_for_frontier(&frontier).is_none() {
      self.loro_snapshots.push(LoroSnapshotChunk {
        snapshot_id: Uuid::new_v4().as_u128(),
        frontier: frontier.clone(),
        version_vector: encode_version_vector(&revision_doc.state_vv()),
        bytes: revision_doc
          .export(ExportMode::Snapshot)
          .map_err(loro_io_error)?,
        created_at_unix_secs: unix_time_secs(),
      });
    }
    self.revisions.push(revision);
    self.manifest.modified_at_unix_secs = unix_time_secs();
    // §19: a revision may add a snapshot chunk; keep the integrity index complete.
    self.integrity_index = self.build_integrity_index();
    self.validate()?;
    Ok(revision_id)
  }

  pub fn load_revision_loro_doc(&self, revision_id: u128) -> io::Result<LoroDoc> {
    let revision = self
      .revisions
      .iter()
      .find(|revision| revision.revision_id == revision_id)
      .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Flowstate package revision is missing"))?;
    if let Some(snapshot) = self.snapshot_for_frontier(&revision.frontier) {
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
      let revision_doc = doc.fork_at(&frontiers).map_err(loro_io_error)?;
      let version_vector = encode_version_vector(&revision_doc.state_vv());
      if self.snapshot_for_frontier(&frontier).is_none() {
        self.loro_snapshots.push(LoroSnapshotChunk {
          snapshot_id: Uuid::new_v4().as_u128(),
          frontier: frontier.clone(),
          version_vector: version_vector.clone(),
          bytes: revision_doc
            .export(ExportMode::Snapshot)
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
    let snapshot_id = self.compact_to_snapshot(doc)?;
    let revision_id = self.create_named_revision_with_id(doc, revision_id, title, summary, author_user_id, replica_id)?;
    Ok((revision_id, snapshot_id))
  }

  pub fn compact_to_snapshot(&mut self, doc: &LoroDoc) -> io::Result<u128> {
    doc.commit();
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
    *self = next;
    Ok(snapshot_id)
  }

  pub fn compact_update_segments_if_needed(&mut self, doc: &LoroDoc, max_update_segments: usize) -> io::Result<Option<u128>> {
    if max_update_segments == 0 || self.loro_update_segments.len() <= max_update_segments {
      return Ok(None);
    }
    self.compact_to_snapshot(doc).map(Some)
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
    let bytes = fs::read(path)?;
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

  pub fn write(&self, path: impl AsRef<Path>) -> io::Result<()> {
    let path = path.as_ref();
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
    let Some(segment) = self.loro_update_segments.last().cloned() else {
      return self.write(path);
    };
    if !file_has_journal_header(path)? {
      return self.write(path);
    }
    let payload = encode_journal_delta(&PackageJournalDelta::Update {
      manifest: self.manifest.clone(),
      segment,
    })?;
    append_journal_transaction(path, &payload)
  }

  pub fn append_latest_update_to_prepared_path(&self, path: impl AsRef<Path>) -> io::Result<()> {
    let path = path.as_ref();
    let Some(segment) = self.loro_update_segments.last().cloned() else {
      return self.write(path);
    };
    if !file_has_journal_header(path)? {
      return self.write(path);
    }
    let payload = encode_journal_delta(&PackageJournalDelta::Update {
      manifest: self.manifest.clone(),
      segment,
    })?;
    append_journal_transaction_to_prepared_file(path, &payload)
  }

  pub fn append_assets_to_path(&self, path: impl AsRef<Path>) -> io::Result<()> {
    let path = path.as_ref();
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
    let mut package = None;
    for payload in committed_journal_payloads(bytes)? {
      if payload.starts_with(PACKAGE_MAGIC) {
        package = Some(Self::from_compact_bytes(payload)?);
        continue;
      }
      let delta = decode_journal_delta(payload)?;
      let current = package
        .as_mut()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Flowstate package journal delta precedes a full generation"))?;
      match delta {
        PackageJournalDelta::Update { manifest, segment } => {
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
    package.validate()?;
    Ok(package)
  }

  fn from_compact_bytes(bytes: &[u8]) -> io::Result<Self> {
    let chunks = read_chunks(bytes)?;
    let mut manifest = None;
    let mut loro_snapshots = Vec::new();
    let mut loro_update_segments = Vec::new();
    let mut assets = Vec::new();
    let mut revisions = Vec::new();
    let mut projection_caches = Vec::new();
    let mut search_units = Vec::new();
    let mut thumbnails = Vec::new();
    let mut integrity_index = Vec::new();

    for chunk in chunks {
      match chunk.kind {
        CHUNK_MANIFEST => manifest = Some(decode_chunk(&chunk.payload, "manifest")?),
        CHUNK_LORO_SNAPSHOT => loro_snapshots.push(decode_chunk(&chunk.payload, "Loro snapshot")?),
        CHUNK_LORO_UPDATE_SEGMENT => loro_update_segments.push(decode_chunk(&chunk.payload, "Loro update segment")?),
        CHUNK_ASSET => assets.push(decode_chunk(&chunk.payload, "asset")?),
        CHUNK_REVISION_INDEX => revisions.push(decode_chunk(&chunk.payload, "revision")?),
        CHUNK_PROJECTION_CACHE => projection_caches.push(decode_chunk(&chunk.payload, "projection cache")?),
        CHUNK_SEARCH_UNIT => search_units.push(decode_chunk(&chunk.payload, "search unit")?),
        CHUNK_THUMBNAIL => thumbnails.push(decode_chunk(&chunk.payload, "thumbnail")?),
        CHUNK_INTEGRITY_INDEX => integrity_index.push(decode_chunk(&chunk.payload, "integrity index entry")?),
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
    };
    package.validate()?;
    Ok(package)
  }

  pub fn to_bytes(&self) -> io::Result<Vec<u8>> {
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

  pub fn validate(&self) -> io::Result<()> {
    if self.manifest.package_format_version != LORO_PACKAGE_FORMAT_VERSION {
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
    for cache in &self.projection_caches {
      validate_frontiers(&cache.frontier, "projection cache frontier")?;
      decode_chunk::<crate::loro_projection::ProjectionBlocks>(&cache.bytes, "projection cache payload")?;
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
    self.load_loro_doc_unvalidated()?;
    Ok(())
  }

  pub fn rebuild_projection_cache_from_loro(&mut self, doc: &LoroDoc) -> io::Result<()> {
    doc.commit();
    let frontier = encode_frontiers(&doc.state_frontiers());
    let projection = crate::loro_projection::projection_blocks_from_loro(doc)?;
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
          .map(|snapshot| (blake3_hash(&snapshot.bytes), snapshot.bytes.len() as u64)),
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

  fn latest_snapshot(&self) -> Option<&LoroSnapshotChunk> {
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
        checksum: blake3_hash(&snapshot.bytes),
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

fn read_chunks(bytes: &[u8]) -> io::Result<Vec<Chunk>> {
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
    let payload = bytes[start..end].to_vec();
    if blake3_hash(&payload) != entry.checksum {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "Flowstate package chunk checksum mismatch"));
    }
    chunks.push(Chunk { kind: entry.kind, payload });
  }
  Ok(chunks)
}

fn cached_search_units_from_compact_bytes(bytes: &[u8]) -> io::Result<(DocumentPackageManifest, Vec<SearchUnitChunk>)> {
  let chunks = read_chunks(bytes)?;
  let mut manifest = None;
  let mut search_units = Vec::new();
  for chunk in chunks {
    match chunk.kind {
      CHUNK_MANIFEST => manifest = Some(decode_chunk(&chunk.payload, "manifest")?),
      CHUNK_SEARCH_UNIT => search_units.push(decode_chunk(&chunk.payload, "search unit")?),
      _ => {},
    }
  }
  let manifest = manifest.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Flowstate package has no manifest"))?;
  Ok((manifest, search_units))
}

fn cached_search_units_from_journal_bytes(bytes: &[u8]) -> io::Result<(DocumentPackageManifest, Vec<SearchUnitChunk>)> {
  let mut cached = None;
  for payload in committed_journal_payloads(bytes)? {
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
  *blake3::hash(bytes).as_bytes()
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
  let parent = path
    .parent()
    .filter(|parent| !parent.as_os_str().is_empty())
    .unwrap_or_else(|| Path::new("."));
  fs::create_dir_all(parent)?;
  let mut file = OpenOptions::new().append(true).open(path)?;
  let mut bytes = Vec::with_capacity(journal_transaction_len(payload.len()));
  append_journal_transaction_bytes(&mut bytes, payload);
  file.write_all(&bytes)?;
  file.sync_all()
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
    if blake3_hash(payload) != checksum {
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
    AssetId, AssetRecord, Block, InputBlock, InputBlockAlignment, InputEquationBlock, InputEquationDisplay, InputEquationSyntax,
    InputImageBlock, InputImageSizing, InputParagraph, InputRun, InputTableBlock, InputTableCell, InputTableCellBlock, InputTableColumnWidth,
    InputTableRow, InputTableStyle, RunStyles, TableCellBlock, document_from_loro, document_to_loro,
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
          cells: vec![
            InputTableCell {
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
        column_widths: vec![InputTableColumnWidth::Auto, InputTableColumnWidth::Auto],
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
      rows: vec![InputTableRow {
        cells: vec![InputTableCell {
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
      column_widths: vec![InputTableColumnWidth::Auto],
      style: InputTableStyle { header_row: false },
    };
    let source = crate::document_from_input_blocks(
      crate::flowstate_document_theme(),
      vec![InputBlock::Table(InputTableBlock {
        rows: vec![InputTableRow {
          cells: vec![InputTableCell {
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
        column_widths: vec![InputTableColumnWidth::Auto],
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
        }),
        InputBlock::Equation(InputEquationBlock {
          source: "x^2".to_string(),
          syntax: InputEquationSyntax::Latex,
          display: InputEquationDisplay::Display,
        }),
        InputBlock::Table(InputTableBlock {
          rows: vec![InputTableRow {
            cells: vec![InputTableCell {
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
          column_widths: vec![InputTableColumnWidth::Auto],
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
        }),
        InputBlock::Equation(InputEquationBlock {
          source: "x^2".to_string(),
          syntax: InputEquationSyntax::Latex,
          display: InputEquationDisplay::InlineLikeParagraph,
        }),
        InputBlock::Table(InputTableBlock {
          rows: vec![InputTableRow {
            cells: vec![InputTableCell {
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
          column_widths: vec![InputTableColumnWidth::FixedPx(144)],
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
          && matches!(table.column_widths.as_slice(), [crate::TableColumnWidth::FixedPx(144)])
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
}
