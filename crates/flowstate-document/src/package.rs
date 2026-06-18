use std::{
  fs,
  io::{self, Cursor, Read as _, Write as _},
  path::Path,
};

use loro::{ExportMode, Frontiers, LoroDoc, VersionVector, cursor::Side};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use uuid::Uuid;

pub const LORO_PACKAGE_FORMAT_VERSION: u32 = 1;
pub const LORO_SCHEMA_VERSION: u32 = 1;

const PACKAGE_MAGIC: &[u8; 16] = b"FLOWDB8-LORO\0\0\0\0";
const PACKAGE_HEADER_VERSION: u32 = 1;

const CHUNK_MANIFEST: u32 = 1;
const CHUNK_LORO_SNAPSHOT: u32 = 2;
const CHUNK_LORO_UPDATE_SEGMENT: u32 = 3;
const CHUNK_ASSET: u32 = 4;
const CHUNK_REVISION_INDEX: u32 = 5;
const CHUNK_PROJECTION_CACHE: u32 = 6;
const CHUNK_SEARCH_UNIT: u32 = 7;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DocumentPackage {
  pub manifest: DocumentPackageManifest,
  pub loro_snapshots: Vec<LoroSnapshotChunk>,
  pub loro_update_segments: Vec<LoroUpdateSegmentChunk>,
  pub assets: Vec<AssetChunk>,
  pub revisions: Vec<PackageRevision>,
  pub projection_caches: Vec<ProjectionCacheChunk>,
  pub search_units: Vec<SearchUnitChunk>,
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
  pub heading_path: Vec<String>,
  pub heading: String,
  pub body: String,
  pub insert_text: String,
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

impl DocumentPackage {
  pub fn from_loro_snapshot(doc: &LoroDoc, title: &str) -> io::Result<Self> {
    Self::from_loro_snapshot_with_assets(doc, title, Vec::new())
  }

  pub fn from_loro_snapshot_with_assets(doc: &LoroDoc, title: &str, assets: Vec<AssetChunk>) -> io::Result<Self> {
    doc.commit();
    let now = unix_time_secs();
    let document_id = Uuid::new_v4().as_u128();
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
      },
      loro_snapshots: vec![LoroSnapshotChunk {
        snapshot_id,
        frontier: frontier.clone(),
        version_vector: version_vector.clone(),
        bytes: snapshot,
        created_at_unix_secs: now,
      }],
      loro_update_segments: Vec::new(),
      assets,
      revisions: vec![PackageRevision {
        revision_id: Uuid::new_v4().as_u128(),
        frontier,
        version_vector,
        title: title.to_string(),
        summary: "Initial snapshot".to_string(),
        author_user_id: None,
        replica_id: None,
        created_at_unix_secs: now,
      }],
      projection_caches: Vec::new(),
      search_units: Vec::new(),
    }
    .with_manifest_indexes()?;
    package.rebuild_search_units_from_loro(doc)?;
    Ok(package)
  }

  pub fn load_loro_doc(&self) -> io::Result<LoroDoc> {
    self.validate()?;
    let snapshot = self
      .latest_snapshot()
      .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Flowstate package has no latest Loro snapshot"))?;
    let doc = LoroDoc::new();
    doc.import(&snapshot.bytes).map_err(loro_io_error)?;
    for segment in &self.loro_update_segments {
      doc.import(&segment.bytes).map_err(loro_io_error)?;
    }
    Ok(doc)
  }

  pub fn current_search_units(&self) -> &[SearchUnitChunk] {
    if self.manifest.search_cache_frontier.as_deref() == Some(self.manifest.latest_frontier.as_slice()) {
      &self.search_units
    } else {
      &[]
    }
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
    self.manifest.search_cache_frontier = None;
    self.search_units.clear();
    self.manifest.modified_at_unix_secs = now;
    self.clone().with_manifest_indexes()?.validate()?;
    *self = self.clone().with_manifest_indexes()?;
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
    doc.commit();
    let frontier = encode_frontiers(&doc.state_frontiers());
    let version_vector = encode_version_vector(&doc.state_vv());
    if frontier != self.manifest.latest_frontier || version_vector != self.manifest.latest_version_vector {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "cannot create a package revision for a Loro state that has not been persisted into the package",
      ));
    }
    let revision = PackageRevision {
      revision_id: Uuid::new_v4().as_u128(),
      frontier: frontier.clone(),
      version_vector,
      title: title.into(),
      summary: summary.into(),
      author_user_id,
      replica_id,
      created_at_unix_secs: unix_time_secs(),
    };
    let revision_id = revision.revision_id;
    if self.snapshot_for_frontier(&frontier).is_none() {
      let revision_doc = doc.fork_at(&doc.state_frontiers()).map_err(loro_io_error)?;
      self.loro_snapshots.push(LoroSnapshotChunk {
        snapshot_id: Uuid::new_v4().as_u128(),
        frontier,
        version_vector: encode_version_vector(&revision_doc.state_vv()),
        bytes: revision_doc.export(ExportMode::Snapshot).map_err(loro_io_error)?,
        created_at_unix_secs: unix_time_secs(),
      });
    }
    self.revisions.push(revision);
    self.manifest.modified_at_unix_secs = unix_time_secs();
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
    let snapshot_id = self.compact_to_snapshot(doc)?;
    let revision_id = self.create_named_revision(doc, title, summary, author_user_id, replica_id)?;
    Ok((revision_id, snapshot_id))
  }

  pub fn compact_to_snapshot(&mut self, doc: &LoroDoc) -> io::Result<u128> {
    doc.commit();
    let snapshot_id = Uuid::new_v4().as_u128();
    let now = unix_time_secs();
    let frontier = encode_frontiers(&doc.state_frontiers());
    let version_vector = encode_version_vector(&doc.state_vv());
    let bytes = doc.export(ExportMode::Snapshot).map_err(loro_io_error)?;
    self.loro_snapshots.push(LoroSnapshotChunk {
      snapshot_id,
      frontier: frontier.clone(),
      version_vector,
      bytes,
      created_at_unix_secs: now,
    });
    self.loro_update_segments.clear();
    self.manifest.latest_snapshot_id = snapshot_id;
    self.manifest.latest_frontier = frontier;
    self.manifest.latest_version_vector = encode_version_vector(&doc.state_vv());
    self.manifest.modified_at_unix_secs = now;
    *self = self.clone().with_manifest_indexes()?;
    self.rebuild_search_units_from_loro(doc)?;
    Ok(snapshot_id)
  }

  pub fn rebuild_search_units_from_loro(&mut self, doc: &LoroDoc) -> io::Result<()> {
    doc.commit();
    let frontier = encode_frontiers(&doc.state_frontiers());
    let body = crate::loro_schema::body_text(doc);
    let body_string = body.to_string();
    let mut units = Vec::new();
    let mut paragraph_start = None;
    let mut paragraph_ix = 0_usize;
    let mut current = String::new();

    for (unicode_ix, ch) in body_string.chars().enumerate() {
      if ch == '\n' {
        if let Some(start) = paragraph_start.take() {
          push_search_unit(
            &mut units,
            &frontier,
            &body,
            self.manifest.document_id,
            paragraph_ix,
            start,
            unicode_ix,
            &current,
          )?;
          paragraph_ix += 1;
          current.clear();
        }
        paragraph_start = Some(unicode_ix + 1);
      } else if paragraph_start.is_some() && ch != crate::OBJECT_REPLACEMENT {
        current.push(ch);
      }
    }

    if let Some(start) = paragraph_start {
      push_search_unit(
        &mut units,
        &frontier,
        &body,
        self.manifest.document_id,
        paragraph_ix,
        start,
        body.len_unicode(),
        &current,
      )?;
    }

    self.search_units = units;
    self.manifest.search_cache_frontier = Some(frontier);
    self.manifest.modified_at_unix_secs = unix_time_secs();
    self.validate()?;
    Ok(())
  }

  pub fn read(path: impl AsRef<Path>) -> io::Result<Self> {
    Self::from_bytes(&fs::read(path)?)
  }

  pub fn write(&self, path: impl AsRef<Path>) -> io::Result<()> {
    write_bytes_atomic(path.as_ref(), &self.to_bytes()?)
  }

  pub fn from_bytes(bytes: &[u8]) -> io::Result<Self> {
    let chunks = read_chunks(bytes)?;
    let mut manifest = None;
    let mut loro_snapshots = Vec::new();
    let mut loro_update_segments = Vec::new();
    let mut assets = Vec::new();
    let mut revisions = Vec::new();
    let mut projection_caches = Vec::new();
    let mut search_units = Vec::new();

    for chunk in chunks {
      match chunk.kind {
        CHUNK_MANIFEST => manifest = Some(decode_chunk(&chunk.payload, "manifest")?),
        CHUNK_LORO_SNAPSHOT => loro_snapshots.push(decode_chunk(&chunk.payload, "Loro snapshot")?),
        CHUNK_LORO_UPDATE_SEGMENT => loro_update_segments.push(decode_chunk(&chunk.payload, "Loro update segment")?),
        CHUNK_ASSET => assets.push(decode_chunk(&chunk.payload, "asset")?),
        CHUNK_REVISION_INDEX => revisions.push(decode_chunk(&chunk.payload, "revision")?),
        CHUNK_PROJECTION_CACHE => projection_caches.push(decode_chunk(&chunk.payload, "projection cache")?),
        CHUNK_SEARCH_UNIT => search_units.push(decode_chunk(&chunk.payload, "search unit")?),
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
      return Err(io::Error::new(io::ErrorKind::InvalidData, "latest snapshot frontier does not match manifest"));
    }
    if let Some(last_segment) = self.loro_update_segments.last()
      && last_segment.to_frontier != self.manifest.latest_frontier
    {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "latest update segment frontier does not match manifest"));
    }
    validate_frontiers(&self.manifest.latest_frontier, "manifest latest frontier")?;
    validate_version_vector(&self.manifest.latest_version_vector, "manifest latest version vector")?;
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
        return Err(io::Error::new(io::ErrorKind::InvalidData, "Loro update segment version-vector chain is broken"));
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
    Ok(())
  }

  fn latest_snapshot(&self) -> Option<&LoroSnapshotChunk> {
    self
      .loro_snapshots
      .iter()
      .find(|snapshot| snapshot.snapshot_id == self.manifest.latest_snapshot_id)
  }

  fn snapshot_for_frontier(&self, frontier: &[u8]) -> Option<&LoroSnapshotChunk> {
    self.loro_snapshots.iter().find(|snapshot| snapshot.frontier == frontier)
  }

  fn with_manifest_indexes(mut self) -> io::Result<Self> {
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
    Ok(self)
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
  let mut entries = Vec::with_capacity(chunk_count as usize);
  for _ in 0..chunk_count {
    let kind = read_u32(&mut cursor)?;
    let offset = read_u64(&mut cursor)?;
    let len = read_u64(&mut cursor)?;
    let mut checksum = [0_u8; 32];
    cursor.read_exact(&mut checksum)?;
    entries.push(ChunkEntry {
      kind,
      offset,
      len,
      checksum,
    });
  }
  let mut chunks = Vec::with_capacity(entries.len());
  for entry in entries {
    let start = usize::try_from(entry.offset)
      .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "chunk offset overflows usize"))?;
    let len = usize::try_from(entry.len)
      .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "chunk length overflows usize"))?;
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
    chunks.push(Chunk {
      kind: entry.kind,
      payload,
    });
  }
  Ok(chunks)
}

fn write_chunks(chunks: &[Chunk]) -> io::Result<Vec<u8>> {
  let table_len = chunks.len() * (4 + 8 + 8 + 32);
  let header_len = PACKAGE_MAGIC.len() + 4 + 4 + table_len;
  let payload_len = chunks.iter().map(|chunk| chunk.payload.len()).sum::<usize>();
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
  postcard::from_bytes(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, format!("decoding {label} failed: {error}")))
}

fn push_search_unit(
  units: &mut Vec<SearchUnitChunk>,
  frontier: &[u8],
  body: &loro::LoroText,
  document_id: u128,
  paragraph_ix: usize,
  start: usize,
  end: usize,
  text: &str,
) -> io::Result<()> {
  let normalized = text.trim();
  if normalized.is_empty() {
    return Ok(());
  }
  let start_cursor = body
    .get_cursor(start, Side::Left)
    .map(|cursor| cursor.encode())
    .unwrap_or_default();
  let end_cursor = body
    .get_cursor(end, Side::Right)
    .map(|cursor| cursor.encode())
    .unwrap_or_default();
  let unit_id = stable_search_unit_id(document_id, paragraph_ix, frontier, normalized);
  units.push(SearchUnitChunk {
    frontier: frontier.to_vec(),
    unit_id,
    unit_kind: "paragraph".to_string(),
    heading_path: Vec::new(),
    heading: String::new(),
    body: normalized.to_string(),
    insert_text: normalized.to_string(),
    paragraph_start_cursor: start_cursor,
    paragraph_end_cursor: end_cursor,
  });
  Ok(())
}

fn stable_search_unit_id(document_id: u128, paragraph_ix: usize, frontier: &[u8], body: &str) -> u128 {
  let mut hasher = blake3::Hasher::new();
  hasher.update(&document_id.to_le_bytes());
  hasher.update(&(paragraph_ix as u64).to_le_bytes());
  hasher.update(frontier);
  hasher.update(body.as_bytes());
  let digest = hasher.finalize();
  let mut bytes = [0_u8; 16];
  bytes.copy_from_slice(&digest.as_bytes()[..16]);
  u128::from_le_bytes(bytes)
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

fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
  let parent = path
    .parent()
    .filter(|p| !p.as_os_str().is_empty())
    .unwrap_or_else(|| Path::new("."));
  fs::create_dir_all(parent)?;
  let mut temp = NamedTempFile::new_in(parent)?;
  temp.write_all(bytes)?;
  temp.as_file_mut().sync_all()?;
  let temp_path = temp.into_temp_path();
  #[cfg(target_os = "windows")]
  {
    match fs::remove_file(path) {
      Ok(()) => {},
      Err(error) if error.kind() == io::ErrorKind::NotFound => {},
      Err(error) => return Err(error),
    }
  }
  temp_path.persist(path).map_err(|error| error.error)
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;

  use super::*;
  use crate::{
    AssetId, AssetRecord, Block, InputBlock, InputBlockAlignment, InputEquationBlock, InputEquationDisplay, InputEquationSyntax,
    InputImageBlock, InputImageSizing, InputParagraph, InputRun, InputTableBlock, InputTableCell, InputTableCellBlock, InputTableColumnWidth,
    InputTableRow, InputTableStyle, RunStyles, TableCellBlock, document_to_loro_db8_bytes,
    loro_schema::{body_text, new_loro_document},
    read_db8_bytes,
  };

  #[test]
  fn package_roundtrips_loro_snapshot() -> io::Result<()> {
    let doc = new_loro_document("Roundtrip").map_err(loro_test_error)?;
    let text = body_text(&doc);
    text.insert(text.len_unicode(), "Hello Loro").map_err(loro_test_error)?;
    let bytes = loro_db8_bytes(&doc, "Roundtrip")?;

    let package = DocumentPackage::from_bytes(&bytes)?;
    assert_eq!(package.manifest.package_format_version, LORO_PACKAGE_FORMAT_VERSION);
    assert_eq!(package.manifest.loro_schema_version, LORO_SCHEMA_VERSION);
    assert_eq!(package.loro_snapshots.len(), 1);

    let loaded = package.load_loro_doc()?;
    assert_eq!(body_text(&loaded).to_string(), "\nHello Loro");
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
    let bytes = document_to_loro_db8_bytes(&source, "Public facade")?;
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
    text.insert(text.len_unicode(), "after save").map_err(loro_test_error)?;
    doc.commit();
    let update = doc.export(ExportMode::updates(&from_vv)).map_err(loro_test_error)?;
    package.append_update_segment(&from_frontier, &from_vv, &doc.state_frontiers(), &doc.state_vv(), update)?;

    let bytes = package.to_bytes()?;
    let loaded = DocumentPackage::from_bytes(&bytes)?.load_loro_doc()?;
    assert_eq!(body_text(&loaded).to_string(), "\nafter save");
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
    let update = doc.export(ExportMode::updates(&from_vv)).map_err(loro_test_error)?;
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
    assert_eq!(package.manifest.search_cache_frontier.as_deref(), Some(package.manifest.latest_frontier.as_slice()));
    assert!(!package.search_units[0].paragraph_start_cursor.is_empty());
    assert!(!package.search_units[0].paragraph_end_cursor.is_empty());
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

    let loaded = read_db8_bytes(&document_to_loro_db8_bytes(&source, "Structured")?)?;
    assert_eq!(crate::paragraph_text(&loaded, 0), "before");
    assert_eq!(loaded.assets.assets.get(&asset_id).map(|asset| asset.bytes.as_ref().clone()), Some(asset_bytes));

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

  fn loro_test_error(error: impl std::error::Error + Send + Sync + 'static) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
  }
}
