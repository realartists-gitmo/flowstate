//! Projection between flowtext documents and the shared Loro document.

use anyhow::{Context as _, Result, bail};
use gpui_flowtext::{
  AssetStore, Block, Document, DocumentTheme, InputBlock, document_from_input_blocks, full_document_text, paragraph_byte_range,
};
use loro::{Container, LoroDoc, LoroMap, LoroMovableList, LoroValue, ValueOrContainer};

use crate::{
  SessionId,
  schema::{
    BLOCKS, BlockPayload, DATA, KIND, KIND_EQUATION, KIND_IMAGE, KIND_PARAGRAPH, KIND_TABLE, META, META_SCHEMA, META_SESSION, META_TITLE,
    MarkIntervals, REV, SCHEMA_VERSION, STYLE, apply_mark_intervals, block_from_payload, body_text, configure_text_styles,
    decode_paragraph_style, encode_paragraph_style, input_paragraphs_from_body_text, mark_intervals_from_runs, payload_from_block,
  },
};

pub fn populate_from_document(doc: &LoroDoc, session: SessionId, title: &str, document: &Document) -> Result<()> {
  configure_text_styles(doc);
  populate_meta(doc, session, title)?;
  replace_blocks_from_document(doc, document)?;
  doc.commit();
  Ok(())
}

pub fn replace_blocks_from_document(doc: &LoroDoc, document: &Document) -> Result<()> {
  configure_text_styles(doc);
  replace_body_from_document(doc, document)?;
  let blocks = doc.get_movable_list(BLOCKS);
  clear_blocks(&blocks)?;

  for block in document.blocks.iter() {
    match block {
      Block::Paragraph(paragraph) => {
        insert_paragraph_container(&blocks, blocks.len(), paragraph.style)?;
      },
      Block::Image(_) | Block::Equation(_) | Block::Table(_) => {
        insert_object_container(&blocks, blocks.len(), block, document)?;
      },
    }
  }

  Ok(())
}

pub fn input_blocks_from_loro(doc: &LoroDoc) -> Result<Vec<InputBlock>> {
  configure_text_styles(doc);
  let blocks = doc.get_movable_list(BLOCKS);
  let body_paragraphs = input_paragraphs_from_body_text(&body_text(doc));
  let mut paragraph_ix = 0usize;
  let mut input_blocks = Vec::with_capacity(blocks.len());
  for ix in 0..blocks.len() {
    let map = map_at(&blocks, ix)?;
    input_blocks.push(input_block_from_container(&map, body_paragraphs.get(paragraph_ix))?);
    if map_string(&map, KIND)? == KIND_PARAGRAPH {
      paragraph_ix += 1;
    }
  }
  Ok(input_blocks)
}

pub fn document_from_loro(doc: &LoroDoc, theme: DocumentTheme) -> Result<Document> {
  Ok(document_from_input_blocks(theme, input_blocks_from_loro(doc)?))
}

pub fn input_block_from_container(map: &LoroMap, body_paragraph: Option<&gpui_flowtext::InputParagraph>) -> Result<InputBlock> {
  match map_string(map, KIND)?.as_str() {
    KIND_PARAGRAPH => {
      let style = decode_paragraph_style(map_i64(map, STYLE)?);
      let mut paragraph = body_paragraph.cloned().unwrap_or(gpui_flowtext::InputParagraph {
        style,
        runs: Vec::new(),
      });
      if body_paragraph.is_none() {
        paragraph.style = style;
      }
      Ok(InputBlock::Paragraph(paragraph))
    },
    KIND_IMAGE | KIND_EQUATION | KIND_TABLE => {
      let payload = map_binary(map, DATA)?;
      let payload = postcard::from_bytes::<BlockPayload>(&payload)?;
      Ok(block_from_payload(payload, &AssetStore::default()))
    },
    kind => bail!("unknown collaboration block kind {kind}"),
  }
}

fn populate_meta(doc: &LoroDoc, session: SessionId, title: &str) -> Result<()> {
  let meta = doc.get_map(META);
  meta.insert(META_SCHEMA, SCHEMA_VERSION)?;
  meta.insert(META_SESSION, session.to_string())?;
  meta.insert(META_TITLE, title)?;
  Ok(())
}

fn clear_blocks(blocks: &LoroMovableList) -> Result<()> {
  while !blocks.is_empty() {
    blocks.delete(0, 1)?;
  }
  Ok(())
}

pub fn insert_paragraph_container(
  blocks: &LoroMovableList,
  ix: usize,
  style: gpui_flowtext::ParagraphStyle,
) -> Result<LoroMap> {
  let map = blocks.insert_container(ix, LoroMap::new())?;
  map.insert(KIND, KIND_PARAGRAPH)?;
  map.insert(STYLE, encode_paragraph_style(style))?;
  Ok(map)
}

pub fn insert_object_container(blocks: &LoroMovableList, ix: usize, block: &Block, document: &Document) -> Result<LoroMap> {
  let Some(payload) = payload_from_block(block, &document.assets) else {
    bail!("paragraph block reached object projection path");
  };
  let kind = match &payload {
    BlockPayload::Image { .. } => KIND_IMAGE,
    BlockPayload::Equation { .. } => KIND_EQUATION,
    BlockPayload::Table(_) => KIND_TABLE,
  };
  let map = blocks.insert_container(ix, LoroMap::new())?;
  map.insert(KIND, kind)?;
  map.insert(DATA, LoroValue::Binary(postcard::to_stdvec(&payload)?.into()))?;
  map.insert(REV, 0_i64)?;
  Ok(map)
}

fn map_at(blocks: &LoroMovableList, ix: usize) -> Result<LoroMap> {
  match blocks.get(ix) {
    Some(ValueOrContainer::Container(Container::Map(map))) => Ok(map),
    Some(ValueOrContainer::Value(_)) | Some(ValueOrContainer::Container(_)) | None => {
      bail!("collaboration block row {ix} is not a map container")
    },
  }
}

fn map_string(map: &LoroMap, key: &str) -> Result<String> {
  match map.get(key) {
    Some(ValueOrContainer::Value(LoroValue::String(value))) => Ok(value.to_string()),
    Some(ValueOrContainer::Value(_)) | Some(ValueOrContainer::Container(_)) | None => {
      bail!("collaboration map key {key} is not a string")
    },
  }
}

pub fn replace_body_from_document(doc: &LoroDoc, document: &Document) -> Result<()> {
  let body = body_text(doc);
  let len = body.len_utf8();
  if len > 0 {
    body.delete_utf8(0, len)?;
  }
  let text = full_document_text(document);
  if !text.is_empty() {
    body.insert_utf8(0, &text)?;
    apply_mark_intervals(&body, &document_mark_intervals(document))?;
  }
  Ok(())
}

fn document_mark_intervals(document: &Document) -> MarkIntervals {
  let mut intervals: MarkIntervals = [Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new()];
  for (paragraph_ix, paragraph) in document.paragraphs.iter().enumerate() {
    let paragraph_start = paragraph_byte_range(document, paragraph_ix).start;
    for (target, source) in intervals
      .iter_mut()
      .zip(mark_intervals_from_runs(&paragraph.runs))
    {
      target.extend(
        source
          .into_iter()
          .map(|(range, value)| (paragraph_start + range.start..paragraph_start + range.end, value)),
      );
    }
    if paragraph.style != gpui_flowtext::ParagraphStyle::Normal {
      let range = paragraph_byte_range(document, paragraph_ix);
      if !range.is_empty() {
        intervals[4].push((range, LoroValue::I64(encode_paragraph_style(paragraph.style))));
      }
    }
  }
  intervals
}

fn map_i64(map: &LoroMap, key: &str) -> Result<i64> {
  match map.get(key) {
    Some(ValueOrContainer::Value(LoroValue::I64(value))) => Ok(value),
    Some(ValueOrContainer::Value(_)) | Some(ValueOrContainer::Container(_)) | None => {
      bail!("collaboration map key {key} is not an i64")
    },
  }
}

fn map_binary(map: &LoroMap, key: &str) -> Result<Vec<u8>> {
  match map.get(key) {
    Some(ValueOrContainer::Value(LoroValue::Binary(value))) => Ok(value.to_vec()),
    Some(ValueOrContainer::Value(_)) | Some(ValueOrContainer::Container(_)) | None => {
      bail!("collaboration map key {key} is not binary")
    },
  }
}

pub fn verify_lineage(doc: &LoroDoc, session: SessionId) -> Result<()> {
  let meta = doc.get_map(META);
  let schema = map_i64(&meta, META_SCHEMA).context("collaboration document has no schema version")?;
  if schema != SCHEMA_VERSION {
    bail!("collaboration document schema version {schema} is not supported")
  }
  let stored = map_string(&meta, META_SESSION).context("collaboration document has no session lineage")?;
  if stored != session.to_string() {
    bail!("collaboration document lineage does not match the ticket session")
  }
  Ok(())
}
