pub const fn bump_paragraph_version(paragraph: &mut Paragraph) {
  paragraph.version = paragraph.version.wrapping_add(1);
}

#[hotpath::measure]
#[must_use]
pub fn split_runs_at(runs: &[TextRun], byte: usize) -> (Vec<TextRun>, Vec<TextRun>) {
  let mut left = Vec::new();
  let mut right = Vec::new();
  let mut offset = 0;
  for run in runs {
    let run_start = offset;
    let run_end = offset + run.len;
    offset = run_end;
    if run_end <= byte {
      left.push(run.clone());
    } else if run_start >= byte {
      right.push(run.clone());
    } else {
      let left_len = byte - run_start;
      let right_len = run_end - byte;
      if left_len > 0 {
        left.push(TextRun {
          len: left_len,
          styles: run.styles,
        });
      }
      if right_len > 0 {
        right.push(TextRun {
          len: right_len,
          styles: run.styles,
        });
      }
    }
  }
  (merge_adjacent_runs(left), merge_adjacent_runs(right))
}

#[hotpath::measure]
pub fn split_paragraph_at(document: &mut DocumentProjection, paragraph_ix: usize, byte: usize) {
  let byte = clamp_paragraph_byte_to_char_boundary(document, paragraph_ix, byte);
  let paragraph = document.paragraphs[paragraph_ix].clone();
  let paragraph_range = paragraph_byte_range(document, paragraph_ix);
  let global = paragraph_range.start + byte;
  document.text.insert(global, "\n");
  let (left_runs, right_runs) = split_runs_at(&paragraph.runs, byte);
  let old_end = paragraph_range.end;
  if let Some(target) = paragraphs_mut(document).get_mut(paragraph_ix) {
    target.byte_range = paragraph_range.start..global;
    target.runs = left_runs;
    bump_paragraph_version(target);
  }
  let new_paragraph = Paragraph {
    style: paragraph.style,
    byte_range: global + 1..old_end + 1,
    runs: right_runs,
    version: paragraph.version.wrapping_add(1),
  };
  paragraphs_mut(document).insert(paragraph_ix + 1, new_paragraph.clone());
  insert_paragraph_id(document, paragraph_ix + 1);
  if let Some(block_ix) = block_ix_for_paragraph(document, paragraph_ix) {
    let updated = Block::Paragraph(document.paragraphs[paragraph_ix].clone());
    let mut blocks = document.blocks.make_mut();
    if let Some(block) = blocks.get_mut(block_ix) {
      *block = updated;
    }
    blocks.insert(block_ix + 1, Block::Paragraph(new_paragraph));
    drop(blocks);
    insert_block_id(document, block_ix + 1);
  }
  rebuild_document_offset_index(document);
  rebuild_document_sections(document);
}

#[hotpath::measure]
pub fn delete_cross_paragraph_range(document: &mut DocumentProjection, range: Range<DocumentOffset>) {
  if range.start.paragraph >= range.end.paragraph {
    delete_range_in_paragraph(document, range.start.paragraph, range.start.byte..range.end.byte);
    return;
  }

  let start_ix = range.start.paragraph;
  let end_ix = range.end.paragraph;
  let start_byte = clamp_paragraph_byte_to_char_boundary(document, start_ix, range.start.byte);
  let end_byte = clamp_paragraph_byte_to_char_boundary(document, end_ix, range.end.byte);
  let start_para = document.paragraphs[start_ix].clone();
  let end_para = document.paragraphs[end_ix].clone();
  let start_para_range = paragraph_byte_range(document, start_ix);
  let end_para_range = paragraph_byte_range(document, end_ix);
  let start_global = start_para_range.start + start_byte;
  let end_global = end_para_range.start + end_byte;
  let delete_len = end_global - start_global;

  let (left_runs, _) = split_runs_at(&start_para.runs, start_byte);
  let (_, right_runs) = split_runs_at(&end_para.runs, end_byte);
  document.text.delete(start_global..end_global);

  let mut merged_runs = left_runs;
  merged_runs.extend(right_runs);
  let paragraphs = paragraphs_mut(document);
  if let Some(target) = paragraphs.get_mut(start_ix) {
    target.runs = merge_adjacent_runs(merged_runs);
    let new_len = paragraph_runs_len(target);
    target.byte_range = start_para_range.start..start_para_range.start + new_len;
    bump_paragraph_version(target);
  }
  paragraphs.splice(start_ix + 1..end_ix + 1, Vec::new());
  remove_paragraph_ids(document, start_ix + 1..end_ix + 1);
  let replacement = document.paragraphs[start_ix].clone();
  replace_paragraph_blocks(document, start_ix, end_ix - start_ix + 1, &[replacement]);
  let _ = delete_len;
  rebuild_document_offset_index(document);
  rebuild_document_sections(document);
}
