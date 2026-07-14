//! One-off remediation: restore a .db8 to its LATEST SNAPSHOT state by
//! dropping all update segments (used to strip accidental headless-soak
//! edits appended after a save; the pre-soak package had zero segments).
use flowstate_document::DocumentPackage;

fn main() -> anyhow::Result<()> {
  let input = std::env::args()
    .nth(1)
    .expect("usage: pkg_restore <in.db8> <out.db8>");
  let output = std::env::args()
    .nth(2)
    .expect("usage: pkg_restore <in.db8> <out.db8>");
  let mut package = DocumentPackage::read(&input)?;
  let snapshot = package
    .latest_snapshot()
    .ok_or_else(|| anyhow::anyhow!("package has no snapshot"))?
    .clone();
  let dropped = package.loro_update_segments.len();
  package.loro_update_segments.clear();
  package.manifest.latest_frontier = snapshot.frontier.clone();
  package.manifest.latest_version_vector = snapshot.version_vector.clone();
  package.manifest.projection_cache_frontier = None;
  package.projection_caches.clear();
  package.manifest.search_cache_frontier = None;
  package.search_units.clear();
  package.refresh_manifest_indexes_public();
  package.validate()?;
  package.write(&output)?;
  println!("restored to snapshot frontier; dropped {dropped} update segments");
  let restored = DocumentPackage::read(&output)?;
  let doc = restored.load_loro_doc()?;
  let projection = flowstate_document::document_from_loro(&doc)?;
  println!(
    "verified: {} paragraphs, {} body chars",
    projection.paragraphs.len(),
    flowstate_document::loro_schema::body_text(&doc)
      .to_string()
      .chars()
      .count()
  );
  Ok(())
}
