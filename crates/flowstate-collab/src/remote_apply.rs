//! Translation from Loro diffs into flowtext collaboration patches.

use anyhow::{Context as _, Result, bail};
use gpui_flowtext::{
  BlockId, CollabPatch, CollabStructuralBlock, CollabTextDelta, Document, InputBlock, ParagraphId, new_block_id, new_paragraph_id,
  paragraph_text,
};
use loro::{
  Container, ContainerID, ContainerTrait as _, LoroDoc, LoroMap, LoroText, LoroValue, TextDelta, ValueOrContainer,
  event::{Diff, DiffEvent, ListDiffItem},
};

use crate::{
  binding::{BindingRow, BlockKind, DocBinding},
  projection::input_block_from_container,
  schema::{BLOCKS, DATA, REV, STYLE, TEXT, decode_paragraph_style},
};

pub struct RemoteApplier<'s> {
  pub doc: &'s LoroDoc,
  pub binding: &'s mut DocBinding,
}

impl RemoteApplier<'_> {
  pub fn apply_event(&mut self, document: &Document, event: &DiffEvent<'_>) -> Result<Vec<CollabPatch>> {
    if !should_process_event(event) {
      return Ok(Vec::new());
    }

    let mut patches = Vec::new();
    for diff in &event.events {
      match &diff.diff {
        Diff::Text(delta) => self.apply_text_diff(document, diff.target, delta, &mut patches)?,
        Diff::Map(delta) => {
          for key in delta.updated.keys() {
            self.apply_map_diff(diff.target, key.as_ref(), &mut patches)?;
          }
        },
        Diff::List(delta) => self.apply_list_diff(diff.target, delta, &mut patches)?,
        Diff::Tree(_) | Diff::Counter(_) | Diff::Unknown => {},
      }
    }
    Ok(patches)
  }

  fn apply_text_diff(&self, document: &Document, target: &ContainerID, delta: &[TextDelta], patches: &mut Vec<CollabPatch>) -> Result<()> {
    let Some(row_ix) = self.binding.by_container.get(target).copied() else {
      return Ok(());
    };
    let row = self
      .binding
      .rows
      .get(row_ix)
      .context("text diff row is outside DocBinding")?;
    if !matches!(row.kind, BlockKind::Paragraph) {
      return Ok(());
    }
    let text = row
      .text
      .as_ref()
      .context("paragraph diff row is missing its LoroText")?;
    let style = decode_paragraph_style(map_i64(&row.map, STYLE)?);
    let paragraph_ix = paragraph_ordinal_for_row(self.binding, row_ix).context("paragraph diff row has no paragraph ordinal")?;
    let old_text = paragraph_text(document, paragraph_ix);
    patches.push(CollabPatch::ParagraphText {
      row: row_ix,
      new: crate::schema::input_paragraph_from_text(text, style),
      delta_utf8: text_delta_to_collab_utf8(&old_text, delta),
    });
    Ok(())
  }

  fn apply_map_diff(&mut self, target: &ContainerID, key: &str, patches: &mut Vec<CollabPatch>) -> Result<()> {
    let Some(row_ix) = self.binding.by_container.get(target).copied() else {
      return Ok(());
    };
    let row = self
      .binding
      .rows
      .get_mut(row_ix)
      .context("map diff row is outside DocBinding")?;
    match key {
      STYLE if matches!(row.kind, BlockKind::Paragraph) => {
        patches.push(CollabPatch::ParagraphStyle {
          row: row_ix,
          style: decode_paragraph_style(map_i64(&row.map, STYLE)?),
        });
      },
      DATA | REV => {
        let input = input_block_from_container(&row.map)?;
        row.kind = block_kind_for_input(&input);
        row.text = None;
        row.paragraph_id = None;
        row.version = 0;
        patches.push(CollabPatch::ReplaceObjectBlock {
          row: row_ix,
          block: CollabStructuralBlock {
            block_id: row.block_id,
            paragraph_id: None,
            block: input,
          },
        });
      },
      _ => {},
    }
    self.binding.rebuild_indexes();
    Ok(())
  }

  fn apply_list_diff(&mut self, target: &ContainerID, delta: &[ListDiffItem], patches: &mut Vec<CollabPatch>) -> Result<()> {
    let blocks = self.doc.get_movable_list(BLOCKS);
    if *target != blocks.id() {
      return Ok(());
    }

    let mut row_ix = 0usize;
    for item in delta {
      match item {
        ListDiffItem::Retain { retain } => row_ix = row_ix.saturating_add(*retain),
        ListDiffItem::Delete { delete } => {
          let count = (*delete).min(self.binding.rows.len().saturating_sub(row_ix));
          if count == 0 {
            continue;
          }
          for _ in 0..count {
            self
              .binding
              .remove_row(row_ix)
              .context("DocBinding row disappeared during remote block delete")?;
          }
          patches.push(CollabPatch::DeleteBlocks { row: row_ix, count });
        },
        ListDiffItem::Insert { insert, is_move } => {
          for value in insert {
            let map = map_from_insert(value)?;
            if *is_move && let Some(from) = self.binding.by_container.get(&map.id()).copied() {
              patches.push(CollabPatch::MoveBlock { from, to: row_ix });
              self.binding.move_row(from, row_ix);
              row_ix += 1;
              continue;
            }

            let input = input_block_from_container(&map)?;
            let structural = structural_block_for_insert(&input);
            let row = binding_row_from_insert(map, &input, structural.block_id, structural.paragraph_id)?;
            self.binding.insert_row(row_ix, row);
            patches.push(CollabPatch::InsertBlocks {
              row: row_ix,
              blocks: vec![structural],
            });
            row_ix += 1;
          }
        },
      }
    }
    Ok(())
  }
}

#[must_use]
pub fn should_process_event(event: &DiffEvent<'_>) -> bool {
  event.triggered_by.is_import() || event.triggered_by.is_checkout()
}

fn text_delta_to_collab_utf8(old_text: &str, delta: &[TextDelta]) -> Vec<CollabTextDelta> {
  let mut output = Vec::with_capacity(delta.len());
  let mut old_chars = 0usize;
  for item in delta {
    match item {
      TextDelta::Retain { retain, .. } => {
        let len = utf8_len_for_char_span(old_text, old_chars, *retain);
        push_text_delta(&mut output, CollabTextDelta::Retain(len));
        old_chars = old_chars.saturating_add(*retain);
      },
      TextDelta::Insert { insert, .. } => push_text_delta(&mut output, CollabTextDelta::Insert(insert.len())),
      TextDelta::Delete { delete } => {
        let len = utf8_len_for_char_span(old_text, old_chars, *delete);
        push_text_delta(&mut output, CollabTextDelta::Delete(len));
        old_chars = old_chars.saturating_add(*delete);
      },
    }
  }
  output
}

fn push_text_delta(output: &mut Vec<CollabTextDelta>, delta: CollabTextDelta) {
  match delta {
    CollabTextDelta::Retain(0) | CollabTextDelta::Insert(0) | CollabTextDelta::Delete(0) => {},
    CollabTextDelta::Retain(len) => match output.last_mut() {
      Some(CollabTextDelta::Retain(existing)) => *existing += len,
      Some(CollabTextDelta::Insert(_)) | Some(CollabTextDelta::Delete(_)) | None => output.push(CollabTextDelta::Retain(len)),
    },
    CollabTextDelta::Insert(len) => match output.last_mut() {
      Some(CollabTextDelta::Insert(existing)) => *existing += len,
      Some(CollabTextDelta::Retain(_)) | Some(CollabTextDelta::Delete(_)) | None => output.push(CollabTextDelta::Insert(len)),
    },
    CollabTextDelta::Delete(len) => match output.last_mut() {
      Some(CollabTextDelta::Delete(existing)) => *existing += len,
      Some(CollabTextDelta::Retain(_)) | Some(CollabTextDelta::Insert(_)) | None => output.push(CollabTextDelta::Delete(len)),
    },
  }
}

fn utf8_len_for_char_span(text: &str, start_char: usize, char_len: usize) -> usize {
  let start = byte_for_char(text, start_char);
  let end = byte_for_char(text, start_char.saturating_add(char_len));
  end.saturating_sub(start)
}

fn byte_for_char(text: &str, target_char: usize) -> usize {
  text
    .char_indices()
    .nth(target_char)
    .map_or(text.len(), |(byte, _)| byte)
}

fn map_from_insert(value: &ValueOrContainer) -> Result<LoroMap> {
  match value {
    ValueOrContainer::Container(Container::Map(map)) => Ok(map.clone()),
    ValueOrContainer::Value(_) | ValueOrContainer::Container(_) => bail!("remote block insert is not a map container"),
  }
}

fn structural_block_for_insert(input: &InputBlock) -> CollabStructuralBlock {
  CollabStructuralBlock {
    block_id: new_block_id(),
    paragraph_id: matches!(input, InputBlock::Paragraph(_)).then(new_paragraph_id),
    block: input.clone(),
  }
}

fn binding_row_from_insert(map: LoroMap, input: &InputBlock, block_id: BlockId, paragraph_id: Option<ParagraphId>) -> Result<BindingRow> {
  let text = if matches!(input, InputBlock::Paragraph(_)) {
    Some(text_child(&map)?)
  } else {
    None
  };
  Ok(BindingRow {
    map,
    text,
    kind: block_kind_for_input(input),
    block_id,
    paragraph_id,
    version: 0,
  })
}

fn block_kind_for_input(input: &InputBlock) -> BlockKind {
  match input {
    InputBlock::Paragraph(_) => BlockKind::Paragraph,
    InputBlock::Image(_) => BlockKind::Image,
    InputBlock::Equation(_) => BlockKind::Equation,
    InputBlock::Table(_) => BlockKind::Table,
  }
}

fn paragraph_ordinal_for_row(binding: &DocBinding, target_row: usize) -> Option<usize> {
  let mut paragraph_ix = 0;
  for (row_ix, row) in binding.rows.iter().enumerate() {
    if row_ix == target_row {
      return row.paragraph_id.map(|_| paragraph_ix);
    }
    if row.paragraph_id.is_some() {
      paragraph_ix += 1;
    }
  }
  None
}

fn text_child(map: &LoroMap) -> Result<LoroText> {
  match map.get(TEXT) {
    Some(ValueOrContainer::Container(Container::Text(text))) => Ok(text),
    Some(ValueOrContainer::Value(_)) | Some(ValueOrContainer::Container(_)) | None => bail!("paragraph block map is missing its text container"),
  }
}

fn map_i64(map: &LoroMap, key: &str) -> Result<i64> {
  match map.get(key) {
    Some(ValueOrContainer::Value(LoroValue::I64(value))) => Ok(value),
    Some(ValueOrContainer::Value(_)) | Some(ValueOrContainer::Container(_)) | None => bail!("collaboration map key {key} is not an i64"),
  }
}
