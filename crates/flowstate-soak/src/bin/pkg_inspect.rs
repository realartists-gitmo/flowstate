//! Scratch diagnostic: break down a .db8 package's size by chunk class.
use flowstate_document::DocumentPackage;

fn main() -> anyhow::Result<()> {
  let path = std::env::args().nth(1).expect("usage: pkg_inspect <file.db8>");
  let started = std::time::Instant::now();
  let preview = DocumentPackage::read_cached_projection(&path)?;
  println!(
    "phase read_cached_projection (preview fast path): {:?} ({})",
    started.elapsed(),
    preview.map_or("cache stale/absent".to_string(), |document| format!("{} paragraphs", document.paragraphs.len()))
  );
  let started = std::time::Instant::now();
  let package = DocumentPackage::read(&path)?;
  println!("phase read: {:?}", started.elapsed());
  let started = std::time::Instant::now();
  let doc = package.load_loro_doc_from_validated()?;
  println!("phase load_loro_doc_from_validated: {:?}", started.elapsed());
  let started = std::time::Instant::now();
  let projection = flowstate_document::document_from_loro(&doc)?;
  println!("phase document_from_loro: {:?} ({} paragraphs)", started.elapsed(), projection.paragraphs.len());
  let snapshot_bytes: usize = package.loro_snapshots.iter().map(|c| c.bytes.len()).sum();
  let segment_bytes: usize = package.loro_update_segments.iter().map(|c| c.bytes.len()).sum();
  let asset_bytes: usize = package.assets.iter().map(|c| c.bytes.len() + c.metadata.len()).sum();
  let cache_bytes: usize = package.projection_caches.iter().map(|c| c.bytes.len()).sum();
  let search_bytes: usize = package.search_units.iter().map(|c| c.frontier.len() + c.unit_kind.len() + 64).sum();
  let thumb_bytes: usize = package.thumbnails.iter().map(|c| c.bytes.len()).sum();
  println!("snapshots: {} chunks, {} bytes", package.loro_snapshots.len(), snapshot_bytes);
  println!("update segments: {} chunks, {} bytes", package.loro_update_segments.len(), segment_bytes);
  println!("assets: {} chunks, {} bytes", package.assets.len(), asset_bytes);
  println!("revisions: {} entries", package.revisions.len());
  println!("projection caches: {} chunks, {} bytes", package.projection_caches.len(), cache_bytes);
  println!("search units: {} chunks, {} bytes", package.search_units.len(), search_bytes);
  println!("thumbnails: {} chunks, {} bytes", package.thumbnails.len(), thumb_bytes);
  for (ix, snapshot) in package.loro_snapshots.iter().enumerate().take(8) {
    println!("  snapshot[{ix}]: id {} bytes {}", snapshot.snapshot_id, snapshot.bytes.len());
  }
  println!("integrity index: {} entries", package.integrity_index.len());
  // Re-encode each top-level section to find where the file size actually lives.
  let encoded = |label: &str, len: usize| println!("  encoded {label}: {len} bytes");
  encoded("manifest", postcard::to_stdvec(&package.manifest).map(|b| b.len()).unwrap_or(0));
  encoded("snapshots", postcard::to_stdvec(&package.loro_snapshots).map(|b| b.len()).unwrap_or(0));
  encoded("segments", postcard::to_stdvec(&package.loro_update_segments).map(|b| b.len()).unwrap_or(0));
  encoded("assets", postcard::to_stdvec(&package.assets).map(|b| b.len()).unwrap_or(0));
  encoded("revisions", postcard::to_stdvec(&package.revisions).map(|b| b.len()).unwrap_or(0));
  encoded("projection_caches", postcard::to_stdvec(&package.projection_caches).map(|b| b.len()).unwrap_or(0));
  encoded("search_units", postcard::to_stdvec(&package.search_units).map(|b| b.len()).unwrap_or(0));
  encoded("thumbnails", postcard::to_stdvec(&package.thumbnails).map(|b| b.len()).unwrap_or(0));
  encoded("integrity_index", postcard::to_stdvec(&package.integrity_index).map(|b| b.len()).unwrap_or(0));
  encoded("whole package", postcard::to_stdvec(&package).map(|b| b.len()).unwrap_or(0));
  Ok(())
}
