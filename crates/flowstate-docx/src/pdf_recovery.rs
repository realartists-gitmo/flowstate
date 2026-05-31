use std::{
  fs, io,
  path::Path,
};

use lopdf::{Dictionary, Document as PdfDocument, Object, ObjectId, Stream, dictionary};

const PAYLOAD_NAME: &str = "flowstate-source.db8.zst";
const PAYLOAD_DESCRIPTION: &str = "Flowstate DB8 source document";
const PAYLOAD_MIME_TYPE: &str = "application/x-flowstate-db8+zstd";
const PAYLOAD_MAGIC: &[u8; 8] = b"FSDB8ZST";
const PAYLOAD_VERSION: u32 = 1;
const ZSTD_LEVEL: i32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlowstatePdfPayloadInfo {
  pub original_len: u64,
  pub compressed_len: u64,
}

#[hotpath::measure]
pub fn embed_db8_file_in_pdf(input_pdf: impl AsRef<Path>, db8_path: impl AsRef<Path>, output_pdf: impl AsRef<Path>) -> io::Result<FlowstatePdfPayloadInfo> {
  let db8_bytes = fs::read(db8_path)?;
  embed_db8_bytes_in_pdf(input_pdf, &db8_bytes, output_pdf)
}

#[hotpath::measure]
pub fn embed_db8_bytes_in_pdf(
  input_pdf: impl AsRef<Path>,
  db8_bytes: &[u8],
  output_pdf: impl AsRef<Path>,
) -> io::Result<FlowstatePdfPayloadInfo> {
  let output_pdf = output_pdf.as_ref();
  if let Some(parent) = output_pdf.parent().filter(|parent| !parent.as_os_str().is_empty()) {
    fs::create_dir_all(parent)?;
  }

  let payload = encode_payload(db8_bytes)?;
  let info = FlowstatePdfPayloadInfo {
    original_len: db8_bytes.len() as u64,
    compressed_len: payload.len() as u64,
  };

  let mut pdf = PdfDocument::load(input_pdf).map_err(pdf_error)?;
  attach_payload(&mut pdf, payload, db8_bytes.len() as u64)?;
  pdf.save(output_pdf)?;
  Ok(info)
}

#[hotpath::measure]
pub fn extract_db8_bytes_from_pdf(pdf_path: impl AsRef<Path>) -> io::Result<Option<Vec<u8>>> {
  let pdf = PdfDocument::load(pdf_path).map_err(pdf_error)?;
  let Some(payload) = find_payload(&pdf)? else {
    return Ok(None);
  };
  decode_payload(&payload).map(Some)
}

#[hotpath::measure]
fn encode_payload(db8_bytes: &[u8]) -> io::Result<Vec<u8>> {
  let compressed = zstd::bulk::compress(db8_bytes, ZSTD_LEVEL).map_err(io::Error::other)?;
  let mut payload = Vec::with_capacity(PAYLOAD_MAGIC.len() + 4 + 8 + compressed.len());
  payload.extend_from_slice(PAYLOAD_MAGIC);
  payload.extend_from_slice(&PAYLOAD_VERSION.to_be_bytes());
  payload.extend_from_slice(&(db8_bytes.len() as u64).to_be_bytes());
  payload.extend_from_slice(&compressed);
  Ok(payload)
}

#[hotpath::measure]
fn decode_payload(payload: &[u8]) -> io::Result<Vec<u8>> {
  let header_len = PAYLOAD_MAGIC.len() + 4 + 8;
  if payload.len() < header_len || &payload[..PAYLOAD_MAGIC.len()] != PAYLOAD_MAGIC {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "PDF does not contain a Flowstate DB8 payload"));
  }

  let version_start = PAYLOAD_MAGIC.len();
  let version = u32::from_be_bytes(payload[version_start..version_start + 4].try_into().expect("version slice has fixed length"));
  if version != PAYLOAD_VERSION {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      format!("unsupported Flowstate PDF payload version {version}"),
    ));
  }

  let len_start = version_start + 4;
  let original_len = u64::from_be_bytes(payload[len_start..len_start + 8].try_into().expect("length slice has fixed length"));
  let decompressed = zstd::bulk::decompress(&payload[header_len..], original_len as usize).map_err(io::Error::other)?;
  if decompressed.len() as u64 != original_len {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "Flowstate PDF payload length mismatch"));
  }
  Ok(decompressed)
}

#[hotpath::measure]
fn attach_payload(pdf: &mut PdfDocument, payload: Vec<u8>, original_len: u64) -> io::Result<()> {
  let embedded_file_id = pdf.add_object(Stream::new(
    dictionary! {
      "Type" => "EmbeddedFile",
      "Subtype" => Object::Name(PAYLOAD_MIME_TYPE.as_bytes().to_vec()),
      "Params" => dictionary! {
        "Size" => original_len as i64,
      },
    },
    payload,
  ));

  let file_spec_id = pdf.add_object(dictionary! {
    "Type" => "Filespec",
    "F" => Object::string_literal(PAYLOAD_NAME),
    "UF" => Object::string_literal(PAYLOAD_NAME),
    "Desc" => Object::string_literal(PAYLOAD_DESCRIPTION),
    "AFRelationship" => "Source",
    "EF" => dictionary! {
      "F" => Object::Reference(embedded_file_id),
      "UF" => Object::Reference(embedded_file_id),
    },
  });

  upsert_embedded_file_name(pdf, file_spec_id)
}

#[hotpath::measure]
fn upsert_embedded_file_name(pdf: &mut PdfDocument, file_spec_id: ObjectId) -> io::Result<()> {
  let names_id = ensure_catalog_names_dictionary(pdf)?;
  let names = pdf.get_object_mut(names_id).map_err(pdf_error)?.as_dict_mut().map_err(pdf_error)?;
  let embedded_files = match names.get_mut(b"EmbeddedFiles") {
    Ok(Object::Dictionary(dict)) => dict,
    _ => {
      names.set("EmbeddedFiles", dictionary! {});
      names
        .get_mut(b"EmbeddedFiles")
        .map_err(pdf_error)?
        .as_dict_mut()
        .map_err(pdf_error)?
    },
  };

  let entries = match embedded_files.get_mut(b"Names") {
    Ok(Object::Array(entries)) => entries,
    _ => {
      embedded_files.set("Names", Vec::<Object>::new());
      embedded_files
        .get_mut(b"Names")
        .map_err(pdf_error)?
        .as_array_mut()
        .map_err(pdf_error)?
    },
  };

  remove_existing_payload_entry(entries);
  entries.push(Object::string_literal(PAYLOAD_NAME));
  entries.push(Object::Reference(file_spec_id));
  Ok(())
}

#[hotpath::measure]
fn ensure_catalog_names_dictionary(pdf: &mut PdfDocument) -> io::Result<ObjectId> {
  let names_id = match pdf.catalog().map_err(pdf_error)?.get(b"Names") {
    Ok(Object::Reference(id)) => Some(*id),
    _ => None,
  };
  if let Some(names_id) = names_id {
    return Ok(names_id);
  }

  let existing_names = match pdf.catalog_mut().map_err(pdf_error)?.remove(b"Names") {
    Some(Object::Dictionary(dict)) => dict,
    _ => Dictionary::new(),
  };
  let names_id = pdf.add_object(existing_names);
  pdf.catalog_mut().map_err(pdf_error)?.set("Names", Object::Reference(names_id));
  Ok(names_id)
}

#[hotpath::measure]
fn remove_existing_payload_entry(entries: &mut Vec<Object>) {
  let mut index = 0;
  while index + 1 < entries.len() {
    if object_string_equals(&entries[index], PAYLOAD_NAME.as_bytes()) {
      entries.drain(index..=index + 1);
    } else {
      index += 2;
    }
  }
}

#[hotpath::measure]
fn find_payload(pdf: &PdfDocument) -> io::Result<Option<Vec<u8>>> {
  let Ok(embedded_files) = pdf.catalog().and_then(|catalog| catalog.get_deref(b"Names", pdf)) else {
    return Ok(None);
  };
  let Ok(embedded_files) = embedded_files
    .as_dict()
    .and_then(|names| names.get_deref(b"EmbeddedFiles", pdf))
    .and_then(Object::as_dict)
  else {
    return Ok(None);
  };
  let Ok(entries) = embedded_files.get(b"Names").and_then(Object::as_array) else {
    return Ok(None);
  };

  let mut index = 0;
  while index + 1 < entries.len() {
    if object_string_equals(&entries[index], PAYLOAD_NAME.as_bytes()) {
      return payload_from_file_spec(pdf, &entries[index + 1]).map(Some);
    }
    index += 2;
  }

  Ok(None)
}

#[hotpath::measure]
fn payload_from_file_spec(pdf: &PdfDocument, file_spec: &Object) -> io::Result<Vec<u8>> {
  let file_spec = pdf.dereference(file_spec).map_err(pdf_error)?.1.as_dict().map_err(pdf_error)?;
  let stream_object = file_spec
    .get_deref(b"EF", pdf)
    .and_then(Object::as_dict)
    .and_then(|ef| ef.get_deref(b"F", pdf))
    .map_err(pdf_error)?;
  let stream = stream_object.as_stream().map_err(pdf_error)?;
  Ok(stream.content.clone())
}

#[hotpath::measure]
fn object_string_equals(object: &Object, expected: &[u8]) -> bool {
  object.as_str().is_ok_and(|value| value == expected)
}

#[hotpath::measure]
fn pdf_error(error: lopdf::Error) -> io::Error {
  io::Error::other(error)
}
