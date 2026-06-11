use std::collections::{HashSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, SyncSender, TrySendError};

use flowstate_collab::{
  ActorId, DocumentId, FLOW_SOURCE_SCHEMA_VERSION, ReplicaId, blake3_hash, loro_peer_id_for_replica,
};
use serde::{Deserialize, Serialize};

const JOURNAL_MAGIC: &[u8] = b"FSFLOWOUTBOX1\n";
const MAX_JOURNAL_BYTES: u64 = 512 * 1024 * 1024;
const MAX_RECORD_BYTES: usize = 32 * 1024 * 1024;
const COMPACT_RECORD_SLACK: usize = 128;

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct FlowOutboxKey {
  pub hash: [u8; 32],
  pub resulting_frontier: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FlowOutboxEntry {
  pub document_id: DocumentId,
  pub actor_id: ActorId,
  pub replica_id: ReplicaId,
  pub peer_id: u64,
  pub schema_version: u32,
  pub update: Vec<u8>,
  pub base_frontier: Vec<u8>,
  pub resulting_frontier: Vec<u8>,
  pub hash: [u8; 32],
}

impl FlowOutboxEntry {
  #[must_use]
  pub fn key(&self) -> FlowOutboxKey {
    FlowOutboxKey {
      hash: self.hash,
      resulting_frontier: self.resulting_frontier.clone(),
    }
  }

  pub fn validate(&self) -> io::Result<()> {
    if self.schema_version != FLOW_SOURCE_SCHEMA_VERSION {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "outbox entry has incompatible flow schema"));
    }
    if self.update.is_empty() || self.update.len() > MAX_RECORD_BYTES {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "outbox entry update size is invalid"));
    }
    if blake3_hash(&self.update) != self.hash {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "outbox entry update hash mismatch"));
    }
    if self.peer_id != loro_peer_id_for_replica(self.replica_id) {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "outbox entry Loro peer does not match replica identity"));
    }
    if self.base_frontier.is_empty() || self.resulting_frontier.is_empty() {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "outbox entry is missing causal frontiers"));
    }
    Ok(())
  }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
enum JournalRecord {
  Put(FlowOutboxEntry),
  Ack(FlowOutboxKey),
}

/// Append-only crash-recovery journal for exact local Loro updates.
///
/// `accept` writes the complete frame into the OS page cache before returning,
/// but deliberately does not wait for `sync_data`; callers can paint after
/// acceptance. `sync_data` defines the stronger fsync boundary and may run on a
/// background worker. Recovery ignores only an incomplete final frame and
/// rejects corruption in any complete frame.
#[derive(Debug)]
pub struct DurableFlowOutbox {
  path: PathBuf,
  file: File,
  sync_worker: OutboxSyncWorker,
  entries: VecDeque<FlowOutboxEntry>,
  keys: HashSet<FlowOutboxKey>,
  journal_records: usize,
}

impl DurableFlowOutbox {
  pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
    let path = path.as_ref().to_path_buf();
    if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
      fs::create_dir_all(parent)?;
    }
    recover_interrupted_compaction(&path)?;
    let mut file = OpenOptions::new()
      .create(true)
      .read(true)
      .append(true)
      .open(&path)?;
    let len = file.metadata()?.len();
    if len > MAX_JOURNAL_BYTES {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "flow outbox journal exceeds size limit"));
    }
    if len == 0 {
      file.write_all(JOURNAL_MAGIC)?;
      file.flush()?;
    }

    let (entries, keys, journal_records, valid_len) = read_journal(&path)?;
    if valid_len < file.metadata()?.len() {
      drop(file);
      OpenOptions::new().write(true).open(&path)?.set_len(valid_len)?;
      file = OpenOptions::new().create(true).read(true).append(true).open(&path)?;
    }
    remove_file_if_exists(&compaction_path(&path))?;
    remove_file_if_exists(&previous_path(&path))?;
    Ok(Self {
      path,
      file,
      sync_worker: OutboxSyncWorker::new()?,
      entries,
      keys,
      journal_records,
    })
  }

  #[must_use]
  pub fn path(&self) -> &Path {
    &self.path
  }

  #[must_use]
  pub fn pending(&self) -> &VecDeque<FlowOutboxEntry> {
    &self.entries
  }

  pub fn accept(&mut self, entry: FlowOutboxEntry) -> io::Result<bool> {
    entry.validate()?;
    let key = entry.key();
    if self.keys.contains(&key) {
      return Ok(false);
    }
    self.append(&JournalRecord::Put(entry.clone()))?;
    self.keys.insert(key);
    self.entries.push_back(entry);
    self.maybe_compact()?;
    Ok(true)
  }

  pub fn acknowledge(&mut self, key: &FlowOutboxKey) -> io::Result<bool> {
    if !self.keys.contains(key) {
      return Ok(false);
    }
    self.append(&JournalRecord::Ack(key.clone()))?;
    self.keys.remove(key);
    self.entries.retain(|entry| entry.key() != *key);
    self.maybe_compact()?;
    Ok(true)
  }

  pub fn sync_data(&self) -> io::Result<()> {
    self.file.sync_data()
  }

  pub fn compact(&mut self) -> io::Result<()> {
    let temporary = compaction_path(&self.path);
    let previous = previous_path(&self.path);
    remove_file_if_exists(&temporary)?;
    remove_file_if_exists(&previous)?;
    {
      let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)?;
      file.write_all(JOURNAL_MAGIC)?;
      for entry in &self.entries {
        write_record(&mut file, &JournalRecord::Put(entry.clone()))?;
      }
      file.sync_all()
    }?;
    fs::rename(&self.path, &previous)?;
    if let Err(error) = fs::rename(&temporary, &self.path) {
      let _ = fs::rename(&previous, &self.path);
      return Err(error);
    }
    self.file = OpenOptions::new()
      .create(true)
      .read(true)
      .append(true)
      .open(&self.path)?;
    remove_file_if_exists(&previous)?;
    self.sync_worker = OutboxSyncWorker::new()?;
    self.journal_records = self.entries.len();
    Ok(())
  }

  fn append(&mut self, record: &JournalRecord) -> io::Result<()> {
    write_record(&mut self.file, record)?;
    self.file.flush()?;
    self.sync_worker.request(&self.file)?;
    self.journal_records += 1;
    Ok(())
  }

  fn maybe_compact(&mut self) -> io::Result<()> {
    if self.journal_records > self.entries.len().saturating_mul(2) + COMPACT_RECORD_SLACK {
      self.compact()?;
    }
    Ok(())
  }
}

#[derive(Debug)]
struct OutboxSyncWorker {
  requests: SyncSender<File>,
}

impl OutboxSyncWorker {
  fn new() -> io::Result<Self> {
    let (requests, receiver) = mpsc::sync_channel::<File>(1);
    std::thread::Builder::new()
      .name("flowstate-outbox-sync".to_string())
      .spawn(move || {
        while let Ok(file) = receiver.recv() {
          let _ = file.sync_data();
        }
      })?;
    Ok(Self { requests })
  }

  fn request(&self, file: &File) -> io::Result<()> {
    let file = file.try_clone()?;
    match self.requests.try_send(file) {
      Ok(()) | Err(TrySendError::Full(_)) => Ok(()),
      Err(TrySendError::Disconnected(file)) => file.sync_data(),
    }
  }
}

fn write_record(file: &mut File, record: &JournalRecord) -> io::Result<()> {
  let payload = postcard::to_stdvec(record).map_err(invalid_data)?;
  if payload.len() > MAX_RECORD_BYTES {
    return Err(io::Error::new(io::ErrorKind::InvalidInput, "flow outbox record exceeds size limit"));
  }
  let len = u32::try_from(payload.len())
    .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "flow outbox record length overflow"))?;
  file.write_all(&len.to_le_bytes())?;
  file.write_all(&payload)?;
  file.write_all(&blake3_hash(&payload))?;
  Ok(())
}

type JournalState = (
  VecDeque<FlowOutboxEntry>,
  HashSet<FlowOutboxKey>,
  usize,
  u64,
);

fn read_journal(path: &Path) -> io::Result<JournalState> {
  let bytes = fs::read(path)?;
  if !bytes.starts_with(JOURNAL_MAGIC) {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid flow outbox journal magic"));
  }
  let mut entries = VecDeque::new();
  let mut keys = HashSet::new();
  let mut records = 0;
  let mut offset = JOURNAL_MAGIC.len();
  let mut valid_len = offset;
  while offset < bytes.len() {
    let frame_start = offset;
    let Some(len_bytes) = bytes.get(offset..offset + 4) else {
      break;
    };
    let len = u32::from_le_bytes(len_bytes.try_into().expect("four-byte slice")) as usize;
    if len > MAX_RECORD_BYTES {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "flow outbox record exceeds size limit"));
    }
    offset += 4;
    let Some(payload) = bytes.get(offset..offset + len) else {
      break;
    };
    offset += len;
    let Some(hash) = bytes.get(offset..offset + 32) else {
      break;
    };
    offset += 32;
    if blake3_hash(payload) != hash {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!("flow outbox journal checksum mismatch at byte {frame_start}"),
      ));
    }
    let record: JournalRecord = postcard::from_bytes(payload).map_err(invalid_data)?;
    apply_record(&mut entries, &mut keys, record)?;
    records += 1;
    valid_len = offset;
  }
  Ok((entries, keys, records, valid_len as u64))
}

fn apply_record(
  entries: &mut VecDeque<FlowOutboxEntry>,
  keys: &mut HashSet<FlowOutboxKey>,
  record: JournalRecord,
) -> io::Result<()> {
  match record {
    JournalRecord::Put(entry) => {
      entry.validate()?;
      let key = entry.key();
      if keys.insert(key) {
        entries.push_back(entry);
      }
    },
    JournalRecord::Ack(key) => {
      keys.remove(&key);
      entries.retain(|entry| entry.key() != key);
    },
  }
  Ok(())
}

fn invalid_data(error: impl std::fmt::Display) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

fn compaction_path(path: &Path) -> PathBuf {
  sidecar_path(path, "compacting")
}

fn previous_path(path: &Path) -> PathBuf {
  sidecar_path(path, "previous")
}

fn sidecar_path(path: &Path, suffix: &str) -> PathBuf {
  let mut value = OsString::from(path.as_os_str());
  value.push(".");
  value.push(suffix);
  PathBuf::from(value)
}

fn recover_interrupted_compaction(path: &Path) -> io::Result<()> {
  if path.exists() {
    return Ok(());
  }
  let compacting = compaction_path(path);
  let previous = previous_path(path);
  if compacting.exists() && journal_is_complete(&compacting) {
    fs::rename(&compacting, path)?;
    remove_file_if_exists(&previous)?;
    return Ok(());
  }
  if previous.exists() && read_journal(&previous).is_ok() {
    fs::rename(&previous, path)?;
    remove_file_if_exists(&compacting)?;
    return Ok(());
  }
  if compacting.exists() || previous.exists() {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "flow outbox interrupted compaction has no recoverable journal",
    ));
  }
  Ok(())
}

fn journal_is_complete(path: &Path) -> bool {
  let Ok((_, _, _, valid_len)) = read_journal(path) else {
    return false;
  };
  path.metadata().is_ok_and(|metadata| metadata.len() == valid_len)
}

fn remove_file_if_exists(path: &Path) -> io::Result<()> {
  match fs::remove_file(path) {
    Ok(()) => Ok(()),
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
    Err(error) => Err(error),
  }
}

#[cfg(test)]
mod tests {
  use std::sync::atomic::{AtomicU64, Ordering};

  use super::*;

  static NEXT_PATH: AtomicU64 = AtomicU64::new(1);

  fn test_path(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
      "flowstate-{label}-{}-{}.outbox",
      std::process::id(),
      NEXT_PATH.fetch_add(1, Ordering::Relaxed)
    ))
  }

  fn entry(update: &[u8], result: u8) -> FlowOutboxEntry {
    let replica_id = ReplicaId::new();
    FlowOutboxEntry {
      document_id: DocumentId::new(),
      actor_id: ActorId::new(),
      replica_id,
      peer_id: loro_peer_id_for_replica(replica_id),
      schema_version: FLOW_SOURCE_SCHEMA_VERSION,
      update: update.to_vec(),
      base_frontier: vec![result.saturating_sub(1)],
      resulting_frontier: vec![result],
      hash: blake3_hash(update),
    }
  }

  fn write_compacted_journal(path: &Path, entries: &[FlowOutboxEntry]) {
    let mut file = OpenOptions::new()
      .create(true)
      .truncate(true)
      .write(true)
      .open(path)
      .unwrap();
    file.write_all(JOURNAL_MAGIC).unwrap();
    for entry in entries {
      write_record(&mut file, &JournalRecord::Put(entry.clone())).unwrap();
    }
    file.sync_all().unwrap();
  }

  #[test]
  fn accepted_entries_survive_restart_until_acknowledged() {
    let path = test_path("restart");
    let first = entry(b"first", 1);
    let second = entry(b"second", 2);
    let mut outbox = DurableFlowOutbox::open(&path).unwrap();
    assert!(outbox.accept(first.clone()).unwrap());
    assert!(outbox.accept(second.clone()).unwrap());
    assert!(!outbox.accept(first.clone()).unwrap());
    outbox.sync_data().unwrap();
    drop(outbox);
    let mut outbox = DurableFlowOutbox::open(&path).unwrap();
    assert_eq!(outbox.pending().iter().cloned().collect::<Vec<_>>(), vec![first.clone(), second.clone()]);
    assert!(outbox.acknowledge(&first.key()).unwrap());
    outbox.sync_data().unwrap();
    drop(outbox);
    let outbox = DurableFlowOutbox::open(&path).unwrap();
    assert_eq!(outbox.pending().iter().cloned().collect::<Vec<_>>(), vec![second]);
    drop(outbox);
    fs::remove_file(path).unwrap();
  }

  #[test]
  fn recovery_discards_only_an_incomplete_final_frame() {
    let path = test_path("truncated");
    let first = entry(b"first", 1);
    let mut outbox = DurableFlowOutbox::open(&path).unwrap();
    outbox.accept(first.clone()).unwrap();
    drop(outbox);
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(&16_u32.to_le_bytes()).unwrap();
    file.write_all(b"partial").unwrap();
    drop(file);
    let outbox = DurableFlowOutbox::open(&path).unwrap();
    assert_eq!(outbox.pending().front(), Some(&first));
    drop(outbox);
    fs::remove_file(path).unwrap();
  }

  #[test]
  fn recovery_promotes_completed_compaction_after_original_rotation() {
    let path = test_path("promote-compaction");
    let first = entry(b"first", 1);
    let second = entry(b"second", 2);
    let mut outbox = DurableFlowOutbox::open(&path).unwrap();
    outbox.accept(first.clone()).unwrap();
    outbox.sync_data().unwrap();
    drop(outbox);

    let compacting = compaction_path(&path);
    let previous = previous_path(&path);
    write_compacted_journal(&compacting, &[first.clone(), second.clone()]);
    fs::rename(&path, &previous).unwrap();

    let outbox = DurableFlowOutbox::open(&path).unwrap();
    assert_eq!(outbox.pending().iter().cloned().collect::<Vec<_>>(), vec![first, second]);
    assert!(!compacting.exists());
    assert!(!previous.exists());
    drop(outbox);
    fs::remove_file(path).unwrap();
  }

  #[test]
  fn recovery_restores_previous_when_compacting_file_is_incomplete() {
    let path = test_path("restore-previous");
    let first = entry(b"first", 1);
    let mut outbox = DurableFlowOutbox::open(&path).unwrap();
    outbox.accept(first.clone()).unwrap();
    outbox.sync_data().unwrap();
    drop(outbox);

    let compacting = compaction_path(&path);
    let previous = previous_path(&path);
    fs::write(&compacting, [JOURNAL_MAGIC, &16_u32.to_le_bytes(), b"partial"].concat()).unwrap();
    fs::rename(&path, &previous).unwrap();

    let outbox = DurableFlowOutbox::open(&path).unwrap();
    assert_eq!(outbox.pending().front(), Some(&first));
    assert!(!compacting.exists());
    assert!(!previous.exists());
    drop(outbox);
    fs::remove_file(path).unwrap();
  }

  #[test]
  fn compaction_preserves_pending_entries_and_restart_recovery() {
    let path = test_path("compact");
    let first = entry(b"first", 1);
    let second = entry(b"second", 2);
    let mut outbox = DurableFlowOutbox::open(&path).unwrap();
    outbox.accept(first.clone()).unwrap();
    outbox.accept(second.clone()).unwrap();
    outbox.acknowledge(&first.key()).unwrap();

    outbox.compact().unwrap();
    assert_eq!(outbox.pending().front(), Some(&second));
    outbox.sync_data().unwrap();
    drop(outbox);

    let outbox = DurableFlowOutbox::open(&path).unwrap();
    assert_eq!(outbox.pending().front(), Some(&second));
    drop(outbox);
    fs::remove_file(path).unwrap();
  }
}
