use std::{
  collections::HashMap,
  fs::File,
  io::{self, Cursor, Read},
  path::Path,
};

use zip::{CompressionMethod, ZipArchive, ZipWriter, write::FileOptions};

use super::blocks::is_metafile;
use super::xml_postprocess::{SideChannel, rewrite_document_xml};

/// The main document part rewritten by the post-process seam.
const DOCUMENT_PART: &str = "word/document.xml";
const CONTENT_TYPES_PART: &str = "[Content_Types].xml";
const DOCUMENT_RELS_PART: &str = "word/_rels/document.xml.rels";

/// §perf-heaven T8.8: docx-rs writes every embedded image to `word/media/*.png`
/// regardless of its real format. Scan the media parts for Windows metafiles
/// (EMF/WMF) by magic bytes and build a rename map `old_path -> new_path` giving
/// each its correct extension. Empty when there are none (the common case).
#[derive(Default)]
struct MetafileRenames {
  /// `word/media/imageN.png` -> `word/media/imageN.emf` (or `.wmf`).
  by_part: HashMap<String, String>,
  needs_emf: bool,
  needs_wmf: bool,
}

fn is_png_media_part(name: &str) -> bool {
  name.starts_with("word/media/")
    && std::path::Path::new(name)
      .extension()
      .is_some_and(|extension| extension.eq_ignore_ascii_case("png"))
}

fn metafile_renames(package: &[u8]) -> io::Result<MetafileRenames> {
  let mut archive =
    ZipArchive::new(Cursor::new(package)).map_err(|error| io::Error::other(format!("failed to read generated docx package: {error}")))?;
  let mut renames = MetafileRenames::default();
  for index in 0..archive.len() {
    let mut entry = archive
      .by_index(index)
      .map_err(|error| io::Error::other(format!("failed to read generated docx entry: {error}")))?;
    let name = entry.name().to_owned();
    if !is_png_media_part(&name) {
      continue;
    }
    // Only the leading bytes are needed to sniff the magic (robust to short reads).
    let mut head = Vec::with_capacity(64);
    entry.by_ref().take(64).read_to_end(&mut head)?;
    if let Some(ext) = is_metafile(&head) {
      let renamed = format!("{}.{ext}", name.trim_end_matches(".png"));
      renames.by_part.insert(name, renamed);
      match ext {
        "emf" => renames.needs_emf = true,
        "wmf" => renames.needs_wmf = true,
        _ => {},
      }
    }
  }
  Ok(renames)
}

#[hotpath::measure]
pub(super) fn write_recompressed_docx(path: &Path, package: Vec<u8>, side: &SideChannel) -> io::Result<()> {
  // §perf-heaven T8.8: metafile media parts get their real extension so EMF/WMF
  // images round-trip LOSSLESSLY instead of falling back to `[Picture N]` text.
  let MetafileRenames {
    by_part: renames,
    needs_emf,
    needs_wmf,
  } = metafile_renames(&package)?;

  let mut archive =
    ZipArchive::new(Cursor::new(package)).map_err(|error| io::Error::other(format!("failed to read generated docx package: {error}")))?;
  let file = File::create(path)?;
  let mut writer = ZipWriter::new(file);
  for index in 0..archive.len() {
    let mut entry = archive
      .by_index(index)
      .map_err(|error| io::Error::other(format!("failed to read generated docx entry: {error}")))?;
    let name = entry.name().to_owned();
    let mut options = FileOptions::default()
      .compression_method(CompressionMethod::Deflated)
      .last_modified_time(entry.last_modified());
    if let Some(mode) = entry.unix_mode() {
      options = options.unix_permissions(mode);
    }
    if entry.is_dir() {
      writer
        .add_directory(name, options)
        .map_err(|error| io::Error::other(format!("failed to write docx directory: {error}")))?;
      continue;
    }
    // The media-part rename maps `word/media/imageN.png` -> `.emf`/`.wmf`.
    let out_name = renames.get(&name).cloned().unwrap_or_else(|| name.clone());
    if name == DOCUMENT_PART {
      // FS-124/125/127: route the main document part through the OOXML rewrite seam.
      let mut bytes = Vec::with_capacity(entry.size() as usize);
      entry.read_to_end(&mut bytes)?;
      let rewritten = rewrite_document_xml(bytes, side);
      write_bytes(&mut writer, &out_name, options, &rewritten)?;
    } else if !renames.is_empty() && name == CONTENT_TYPES_PART {
      let mut bytes = Vec::new();
      entry.read_to_end(&mut bytes)?;
      write_bytes(&mut writer, &out_name, options, &rewrite_content_types(&bytes, needs_emf, needs_wmf))?;
    } else if name == DOCUMENT_RELS_PART && (!renames.is_empty() || !side.linked_image_rels().is_empty()) {
      let mut bytes = Vec::new();
      entry.read_to_end(&mut bytes)?;
      if !renames.is_empty() {
        bytes = rewrite_rels_targets(&bytes, &renames);
      }
      // §A11.9: append the linked images' `TargetMode="External"` relationships.
      if !side.linked_image_rels().is_empty() {
        bytes = inject_external_image_rels(&bytes, side.linked_image_rels());
      }
      write_bytes(&mut writer, &out_name, options, &bytes)?;
    } else {
      writer
        .start_file(out_name, options)
        .map_err(|error| io::Error::other(format!("failed to write docx entry: {error}")))?;
      io::copy(&mut entry, &mut writer)?;
    }
  }
  writer
    .finish()
    .map_err(|error| io::Error::other(format!("failed to finish docx package: {error}")))?;
  Ok(())
}

fn write_bytes(writer: &mut ZipWriter<File>, name: &str, options: FileOptions, bytes: &[u8]) -> io::Result<()> {
  writer
    .start_file(name, options)
    .map_err(|error| io::Error::other(format!("failed to write docx entry: {error}")))?;
  io::Write::write_all(writer, bytes)
}

/// Add `<Default>` content-type declarations for the metafile extensions we
/// renamed to, so Word resolves `word/media/*.emf`/`.wmf` parts.
fn rewrite_content_types(bytes: &[u8], needs_emf: bool, needs_wmf: bool) -> Vec<u8> {
  let Ok(xml) = std::str::from_utf8(bytes) else {
    return bytes.to_vec();
  };
  let mut inserts = String::new();
  if needs_emf && !xml.contains("Extension=\"emf\"") {
    inserts.push_str(r#"<Default Extension="emf" ContentType="image/x-emf"/>"#);
  }
  if needs_wmf && !xml.contains("Extension=\"wmf\"") {
    inserts.push_str(r#"<Default Extension="wmf" ContentType="image/x-wmf"/>"#);
  }
  if inserts.is_empty() {
    return bytes.to_vec();
  }
  xml
    .replacen("</Types>", &format!("{inserts}</Types>"), 1)
    .into_bytes()
}

/// Rewrite relationship `Target`s for the renamed media parts (the rels are
/// relative to `word/`, so `word/media/imageN.png` -> target `media/imageN.png`).
/// §A11.9 hardening: the replace is scoped to `Target="…"` ATTRIBUTES — the old
/// blind substring replace could rewrite an external URL (or an id) that merely
/// contained a media path.
fn rewrite_rels_targets(bytes: &[u8], renames: &HashMap<String, String>) -> Vec<u8> {
  let Ok(mut xml) = std::str::from_utf8(bytes).map(str::to_owned) else {
    return bytes.to_vec();
  };
  for (old_path, new_path) in renames {
    let old_target = format!("Target=\"{}\"", old_path.trim_start_matches("word/"));
    let new_target = format!("Target=\"{}\"", new_path.trim_start_matches("word/"));
    xml = xml.replace(&old_target, &new_target);
  }
  xml.into_bytes()
}

/// §A11.9: append one `TargetMode="External"` image relationship per linked
/// image to `word/_rels/document.xml.rels`. The `rIdFsLinkN` ids are allocated
/// by [`SideChannel::push_linked_image`] outside docx-rs's numeric `rIdN` range.
fn inject_external_image_rels(bytes: &[u8], rels: &[(String, String)]) -> Vec<u8> {
  let Ok(xml) = std::str::from_utf8(bytes) else {
    return bytes.to_vec();
  };
  use std::fmt::Write as _;
  let mut inserts = String::new();
  for (relationship_id, url) in rels {
    let _ = write!(
      inserts,
      "<Relationship Id=\"{relationship_id}\" \
       Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" \
       Target=\"{}\" TargetMode=\"External\"/>",
      super::xml_postprocess::escape_attr(url),
    );
  }
  if inserts.is_empty() {
    return bytes.to_vec();
  }
  xml
    .replacen("</Relationships>", &format!("{inserts}</Relationships>"), 1)
    .into_bytes()
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::io::Write as _;

  /// A minimal EMF header: `is_metafile` needs `0x01000000` at offset 0 and the
  /// `" EMF"` signature at offset 40.
  fn minimal_emf() -> Vec<u8> {
    let mut bytes = vec![0u8; 88];
    bytes[0..4].copy_from_slice(&[0x01, 0x00, 0x00, 0x00]);
    bytes[40..44].copy_from_slice(b" EMF");
    bytes
  }

  fn zip_with(parts: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut buffer = Cursor::new(Vec::new());
    {
      let mut writer = ZipWriter::new(&mut buffer);
      let options = FileOptions::default().compression_method(CompressionMethod::Deflated);
      for (name, bytes) in parts {
        writer.start_file(*name, options).unwrap();
        writer.write_all(bytes).unwrap();
      }
      writer.finish().unwrap()
    };
    buffer.into_inner()
  }

  /// §perf-heaven T8.8 NET: docx-rs writes every image to `word/media/*.png`, so a
  /// metafile lands there mislabelled. `write_recompressed_docx` must sniff the
  /// magic, rename the part to `.emf`, add the `emf` content-type Default, and
  /// rewrite the relationship Target — so the EMF is a VALID docx image part (a
  /// lossless round-trip) instead of the former `[Picture N]` text.
  #[test]
  fn recompress_renames_metafile_media_and_fixes_refs() {
    let package = zip_with(&[
      (
        "[Content_Types].xml",
        br#"<?xml version="1.0"?><Types xmlns="x"><Default Extension="png" ContentType="image/png"/></Types>"#.to_vec(),
      ),
      (
        "word/_rels/document.xml.rels",
        br#"<?xml version="1.0"?><Relationships><Relationship Id="rId7" Type="t" Target="media/image1.png"/></Relationships>"#.to_vec(),
      ),
      ("word/media/image1.png", minimal_emf()),
      ("word/document.xml", b"<w:document/>".to_vec()),
    ]);

    let dir = std::env::temp_dir();
    let out = dir.join(format!("fs-emf-rezip-{}.docx", std::process::id()));
    write_recompressed_docx(&out, package, &SideChannel::default()).expect("recompress");
    let bytes = std::fs::read(&out).expect("read out");
    let _ = std::fs::remove_file(&out);

    let mut archive = ZipArchive::new(Cursor::new(bytes)).expect("open out");
    let names: Vec<String> = (0..archive.len())
      .map(|i| archive.by_index(i).unwrap().name().to_owned())
      .collect();
    assert!(
      names.iter().any(|n| n == "word/media/image1.emf"),
      "metafile part not renamed to .emf: {names:?}"
    );
    assert!(
      !names.iter().any(|n| n == "word/media/image1.png"),
      "old .png media part still present: {names:?}"
    );

    let read = |archive: &mut ZipArchive<Cursor<Vec<u8>>>, name: &str| -> String {
      let mut entry = archive.by_name(name).unwrap();
      let mut text = String::new();
      entry.read_to_string(&mut text).unwrap();
      text
    };
    assert!(
      read(&mut archive, "[Content_Types].xml").contains(r#"Extension="emf""#),
      "emf content-type not declared"
    );
    let rels = read(&mut archive, "word/_rels/document.xml.rels");
    assert!(rels.contains("media/image1.emf"), "rel target not rewritten to .emf: {rels}");
    assert!(!rels.contains("media/image1.png"), "rel still points to .png: {rels}");
  }

  /// A non-metafile package must be recompressed byte-for-part unchanged (no
  /// spurious renames on the common all-PNG case).
  #[test]
  fn recompress_leaves_png_media_untouched() {
    let png = vec![137, 80, 78, 71, 13, 10, 26, 10, 0, 0];
    let package = zip_with(&[
      ("[Content_Types].xml", b"<Types></Types>".to_vec()),
      ("word/media/image1.png", png.clone()),
      ("word/document.xml", b"<w:document/>".to_vec()),
    ]);
    let out = std::env::temp_dir().join(format!("fs-png-rezip-{}.docx", std::process::id()));
    write_recompressed_docx(&out, package, &SideChannel::default()).expect("recompress");
    let bytes = std::fs::read(&out).unwrap();
    let _ = std::fs::remove_file(&out);
    let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
    let names: Vec<String> = (0..archive.len())
      .map(|i| archive.by_index(i).unwrap().name().to_owned())
      .collect();
    assert!(
      names.iter().any(|n| n == "word/media/image1.png"),
      "png media renamed unexpectedly: {names:?}"
    );
  }
}
