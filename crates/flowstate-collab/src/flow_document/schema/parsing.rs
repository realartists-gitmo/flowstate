use super::*;

#[cfg(test)]
thread_local! {
  static PARSE_FLOW_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(in crate::flow_document) fn parse_flow_call_count() -> usize {
  PARSE_FLOW_CALLS.with(std::cell::Cell::get)
}

#[derive(Clone)]
pub(super) struct RawMark {
  pub range_utf8: Range<usize>,
  pub key: String,
  pub value: FlowMarkValue,
}

pub(super) struct ParsedNode {
  pub node: FlowNode,
  pub token_unicode: usize,
  pub content_utf8: Range<usize>,
}

pub(super) struct ParsedFlow {
  pub materialized: MaterializedFlow,
  pub parsed_nodes: Vec<ParsedNode>,
  pub mark_count: usize,
  pub text_bytes: usize,
}

pub(super) fn parse_flow(doc: &LoroDoc, flow_id: FlowId, limits: &FlowSourceLimits) -> CollabResult<ParsedFlow> {
  #[cfg(test)]
  PARSE_FLOW_CALLS.with(|calls| calls.set(calls.get() + 1));
  let text = flow_text(doc, flow_id)?;
  let raw_text = text.to_string();
  if raw_text.len() > limits.max_flow_text_bytes {
    return Err(CollabError::InvalidSchema("vNext flow text limit"));
  }
  if raw_text.is_empty() {
    return Err(CollabError::InvalidSchema("empty vNext flow"));
  }
  let raw_marks = raw_marks(&text)?;
  let mut mark_cursor = MarkSweep::new(&raw_marks);
  let mut token_specs = Vec::new();
  for (unicode_pos, (byte_pos, ch)) in raw_text.char_indices().enumerate() {
    mark_cursor.advance(byte_pos);
    match ch {
      PARAGRAPH_TOKEN | OBJECT_TOKEN => {
        let kind = if ch == PARAGRAPH_TOKEN {
          FlowNodeKind::Paragraph
        } else {
          FlowNodeKind::Object
        };
        let structural = mark_cursor
          .active()
          .filter(|mark| is_structural_key(&mark.key))
          .collect::<Vec<_>>();
        // Concurrent inline marks may causally span a structural token that
        // another replica inserted inside the marked range. Those user marks
        // are deterministically ignored for the token and clipped out of
        // paragraph projection; only structural-mark ambiguity is invalid.
        if structural.len() != 1 || structural[0].key != kind.mark_key() {
          return Err(CollabError::InvalidSchema("vNext structural token marks"));
        }
        let FlowMarkValue::String(id) = &structural[0].value else {
          return Err(CollabError::InvalidSchema("vNext structural node ID mark"));
        };
        token_specs.push((byte_pos, unicode_pos, kind, parse_node_id(id)?));
      },
      _ => {
        if mark_cursor.active().any(|mark| is_structural_key(&mark.key)) {
          return Err(CollabError::InvalidSchema("vNext structural mark on user text"));
        }
      },
    }
  }
  if token_specs.first().is_none_or(|(byte, unicode, _, _)| *byte != 0 || *unicode != 0) {
    return Err(CollabError::InvalidSchema("vNext flow does not begin with structural token"));
  }

  let mut nodes = Vec::with_capacity(token_specs.len());
  let mut parsed_nodes = Vec::with_capacity(token_specs.len());
  for (index, (token_byte, token_unicode, kind, node_id)) in token_specs.iter().copied().enumerate() {
    let token_end_byte = token_byte + kind.token().len_utf8();
    let content_end_byte = token_specs.get(index + 1).map_or(raw_text.len(), |next| next.0);
    let record = read_node_record(doc, node_id)?;
    if record.kind != kind {
      return Err(CollabError::InvalidSchema("vNext token/record kind mismatch"));
    }
    let node = match kind {
      FlowNodeKind::Paragraph => {
        if !record.child_flows.is_empty() {
          return Err(CollabError::InvalidSchema("vNext paragraph owns child flow"));
        }
        FlowNode::Paragraph {
          record,
          text: raw_text[token_end_byte..content_end_byte].to_string(),
          marks: project_inline_marks(&raw_marks, token_end_byte..content_end_byte),
        }
      },
      FlowNodeKind::Object => {
        if content_end_byte != token_end_byte {
          return Err(CollabError::InvalidSchema("vNext object token has text content"));
        }
        FlowNode::Object { record }
      },
    };
    nodes.push(node.clone());
    parsed_nodes.push(ParsedNode {
      node,
      token_unicode,
      content_utf8: token_end_byte..content_end_byte,
    });
  }
  Ok(ParsedFlow {
    materialized: MaterializedFlow { id: flow_id, nodes },
    parsed_nodes,
    mark_count: raw_marks.len(),
    text_bytes: raw_text.len(),
  })
}

pub(super) fn materialize_flow_window(
  doc: &LoroDoc,
  flow_id: FlowId,
  changed_unicode: Range<usize>,
) -> CollabResult<MaterializedFlowWindow> {
  let text = flow_text(doc, flow_id)?;
  let len = text.len_unicode();
  if len == 0 {
    return Err(CollabError::InvalidSchema("empty vNext flow"));
  }
  let nearest = changed_unicode.start.min(len.saturating_sub(1));
  let containing_start = previous_structural_token(&text, nearest)?;
  let starts_at_structural_token = changed_unicode.start < len
    && matches!(
      text.char_at(changed_unicode.start).map_err(loro_error)?,
      PARAGRAPH_TOKEN | OBJECT_TOKEN
    );
  let start = if starts_at_structural_token && containing_start > 0 {
    previous_structural_token(&text, containing_start - 1)?
  } else {
    containing_start
  };
  let search_end = changed_unicode.end.min(len).max(containing_start + 1);
  let end = next_structural_token(&text, search_end)?.unwrap_or(len);
  let delta = text
    .slice_delta(start, end, loro::cursor::PosType::Unicode)
    .map_err(loro_error)?;
  let (raw_text, raw_marks) = raw_text_and_marks_from_delta(&delta)?;
  let mut mark_cursor = MarkSweep::new(&raw_marks);
  let mut token_specs = Vec::new();
  for (local_unicode, (byte, ch)) in raw_text.char_indices().enumerate() {
    mark_cursor.advance(byte);
    let kind = match ch {
      PARAGRAPH_TOKEN => Some(FlowNodeKind::Paragraph),
      OBJECT_TOKEN => Some(FlowNodeKind::Object),
      _ => None,
    };
    if let Some(kind) = kind {
      let FlowMarkValue::String(id) = mark_cursor
        .active()
        .find(|mark| mark.key == kind.mark_key())
        .map(|mark| &mark.value)
        .ok_or(CollabError::InvalidSchema("vNext structural token mark missing"))?
      else {
        return Err(CollabError::InvalidSchema("vNext structural node ID mark"));
      };
      token_specs.push((byte, local_unicode, kind, parse_node_id(id)?));
    }
  }
  if token_specs.first().is_none_or(|(byte, _, _, _)| *byte != 0) {
    return Err(CollabError::InvalidSchema("vNext materialization window does not begin at a structural token"));
  }
  let mut nodes = Vec::with_capacity(token_specs.len());
  for (index, (token_byte, _, kind, node_id)) in token_specs.iter().copied().enumerate() {
    let token_end = token_byte + kind.token().len_utf8();
    let content_end = token_specs.get(index + 1).map_or(raw_text.len(), |next| next.0);
    let record = read_node_record(doc, node_id)?;
    if record.kind != kind {
      return Err(CollabError::InvalidSchema("vNext token/record kind mismatch"));
    }
    nodes.push(match kind {
      FlowNodeKind::Paragraph => FlowNode::Paragraph {
        record,
        text: raw_text[token_end..content_end].to_string(),
        marks: project_inline_marks(&raw_marks, token_end..content_end),
      },
      FlowNodeKind::Object if token_end == content_end => FlowNode::Object { record },
      FlowNodeKind::Object => return Err(CollabError::InvalidSchema("vNext object token has text content")),
    });
  }
  Ok(MaterializedFlowWindow {
    id: flow_id,
    unicode_range: start..end,
    nodes,
  })
}

fn previous_structural_token(text: &LoroText, mut position: usize) -> CollabResult<usize> {
  loop {
    if matches!(text.char_at(position).map_err(loro_error)?, PARAGRAPH_TOKEN | OBJECT_TOKEN) {
      return Ok(position);
    }
    if position == 0 {
      return Err(CollabError::InvalidSchema("vNext flow does not begin with structural token"));
    }
    position -= 1;
  }
}

fn next_structural_token(text: &LoroText, mut position: usize) -> CollabResult<Option<usize>> {
  while position < text.len_unicode() {
    if matches!(text.char_at(position).map_err(loro_error)?, PARAGRAPH_TOKEN | OBJECT_TOKEN) {
      return Ok(Some(position));
    }
    position += 1;
  }
  Ok(None)
}

fn raw_text_and_marks_from_delta(delta: &[loro::TextDelta]) -> CollabResult<(String, Vec<RawMark>)> {
  let mut raw_text = String::new();
  let mut marks = Vec::new();
  for item in delta {
    let loro::TextDelta::Insert { insert, attributes } = item else {
      return Err(CollabError::InvalidSchema("vNext text slice contains a non-insert delta"));
    };
    let start = raw_text.len();
    raw_text.push_str(insert);
    if let Some(attributes) = attributes {
      for (key, value) in attributes {
        let value = FlowMarkValue::from_loro(value).ok_or(CollabError::InvalidSchema("vNext unsupported text mark value"))?;
        marks.push(RawMark {
          range_utf8: start..raw_text.len(),
          key: key.clone(),
          value,
        });
      }
    }
  }
  marks.sort_by_key(|mark| (mark.range_utf8.start, mark.range_utf8.end));
  Ok((raw_text, marks))
}

pub(super) fn raw_marks(text: &LoroText) -> CollabResult<Vec<RawMark>> {
  let mut offset = 0;
  let mut marks = Vec::new();
  for delta in text.to_delta() {
    let (insert, attributes) = match delta {
      loro::TextDelta::Insert { insert, attributes } => (insert, attributes),
      loro::TextDelta::Retain { retain, .. } | loro::TextDelta::Delete { delete: retain } => {
        offset += retain;
        continue;
      },
    };
    let start = offset;
    offset += insert.len();
    if let Some(attributes) = attributes {
      for (key, value) in attributes {
        let Some(value) = FlowMarkValue::from_loro(&value) else {
          return Err(CollabError::InvalidSchema("vNext unsupported text mark value"));
        };
        marks.push(RawMark {
          range_utf8: start..offset,
          key,
          value,
        });
      }
    }
  }
  marks.sort_by_key(|mark| (mark.range_utf8.start, mark.range_utf8.end));
  Ok(marks)
}

struct MarkSweep<'a> {
  marks: &'a [RawMark],
  next: usize,
  active: Vec<usize>,
}

impl<'a> MarkSweep<'a> {
  fn new(marks: &'a [RawMark]) -> Self {
    Self {
      marks,
      next: 0,
      active: Vec::new(),
    }
  }

  fn advance(&mut self, byte_pos: usize) {
    self.active.retain(|index| byte_pos < self.marks[*index].range_utf8.end);
    while self
      .marks
      .get(self.next)
      .is_some_and(|mark| mark.range_utf8.start <= byte_pos)
    {
      if byte_pos < self.marks[self.next].range_utf8.end {
        self.active.push(self.next);
      }
      self.next += 1;
    }
  }

  fn active(&self) -> impl Iterator<Item = &'a RawMark> + '_ {
    self.active.iter().map(|index| &self.marks[*index])
  }
}

#[allow(dead_code, reason = "available for future use by windowed mark utilities")]
pub(super) fn marks_at(marks: &[RawMark], byte_pos: usize) -> Vec<&RawMark> {
  marks
    .iter()
    .filter(|mark| mark.range_utf8.start <= byte_pos && byte_pos < mark.range_utf8.end)
    .collect()
}

fn project_inline_marks(marks: &[RawMark], content: Range<usize>) -> Vec<FlowInlineMark> {
  marks
    .iter()
    .filter(|mark| !is_structural_key(&mark.key))
    .filter_map(|mark| {
      let start = mark.range_utf8.start.max(content.start);
      let end = mark.range_utf8.end.min(content.end);
      (start < end).then(|| FlowInlineMark {
        range_utf8: start - content.start..end - content.start,
        key: mark.key.clone(),
        value: mark.value.clone(),
      })
    })
    .collect()
}
