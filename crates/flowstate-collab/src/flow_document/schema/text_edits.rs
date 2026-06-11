use super::*;

pub(in crate::flow_document) fn insert_text_with_exact_marks(
  doc: &LoroDoc,
  flow_id: FlowId,
  unicode_pos: usize,
  text: &str,
  marks: &[(String, FlowMarkValue)],
) -> CollabResult<()> {
  if text.is_empty() {
    return Ok(());
  }
  validate_user_text(text)?;
  for (key, _) in marks {
    validate_inline_mark_key(key)?;
  }
  let flow = flow_text(doc, flow_id)?;
  flow.insert(unicode_pos, text).map_err(loro_error)?;
  let unicode_end = unicode_pos + text.chars().count();
  let inherited_keys = flow
    .slice_delta(unicode_pos, unicode_end, PosType::Unicode)
    .map_err(loro_error)?
    .into_iter()
    .flat_map(|delta| match delta {
      loro::TextDelta::Insert {
        attributes: Some(attributes),
        ..
      } => attributes.keys().cloned().collect::<Vec<_>>(),
      loro::TextDelta::Insert { attributes: None, .. }
      | loro::TextDelta::Retain { .. }
      | loro::TextDelta::Delete { .. } => Vec::new(),
    })
    .filter(|key| !is_structural_key(key))
    .collect::<BTreeSet<_>>();
  for key in inherited_keys {
    flow.unmark(unicode_pos..unicode_end, &key).map_err(loro_error)?;
  }
  for (key, value) in marks {
    flow
      .mark(unicode_pos..unicode_end, key, value.clone().into_loro())
      .map_err(loro_error)?;
  }
  Ok(())
}

pub(in crate::flow_document) fn replace_paragraph_text_at(
  doc: &LoroDoc,
  paragraph_id: FlowNodeId,
  flow_id: FlowId,
  token: usize,
  text: &str,
  marks: &[(String, FlowMarkValue)],
) -> CollabResult<()> {
  validate_user_text(text)?;
  for (key, _) in marks {
    validate_inline_mark_key(key)?;
  }
  let range = paragraph_content_range_at(doc, paragraph_id, flow_id, token)?;
  let flow = flow_text(doc, flow_id)?;
  if !range.is_empty() {
    flow.delete(range.start, range.len()).map_err(loro_error)?;
  }
  if !text.is_empty() {
    insert_text_with_exact_marks(doc, flow_id, range.start, text, marks)?;
  }
  Ok(())
}
