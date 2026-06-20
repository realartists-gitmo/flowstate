#[hotpath::measure]
pub fn insert_rich_fragment_at(document: &mut DocumentProjection, offset: DocumentOffset, fragment: &RichClipboardFragment) -> DocumentOffset {
  let Some(first_paragraph) = fragment.paragraphs.first() else {
    return offset;
  };
  if fragment.paragraphs.len() == 1 {
    return insert_single_paragraph_fragment_at(document, offset, first_paragraph);
  }
  insert_multi_paragraph_fragment_at(document, offset, fragment)
}

#[hotpath::measure]
fn insert_single_paragraph_fragment_at(document: &mut DocumentProjection, offset: DocumentOffset, paragraph: &InputParagraph) -> DocumentOffset {
  let text = input_paragraph_text(paragraph);
  if text.is_empty() {
    return offset;
  }
  let Some(target) = document.paragraphs.get(offset.paragraph) else {
    return offset;
  };
  let byte = offset.byte.min(paragraph_text_len(target));
  let paragraph_start = paragraph_byte_range(document, offset.paragraph).start;
  let (left_runs, right_runs) = split_runs_at(&target.runs, byte);
  document.text.insert(paragraph_start + byte, &text);
  let mut runs = Vec::with_capacity(left_runs.len() + paragraph.runs.len() + right_runs.len());
  runs.extend(left_runs);
  runs.extend(input_paragraph_text_runs(paragraph));
  runs.extend(right_runs);
  {
    let target = &mut paragraphs_mut(document)[offset.paragraph];
    target.style = paragraph.style;
    target.runs = merge_adjacent_runs(runs);
    bump_paragraph_version(target);
  };
  update_paragraph_offsets_after_len_change(document, offset.paragraph);
  rebuild_document_sections(document);
  DocumentOffset {
    paragraph: offset.paragraph,
    byte: byte + text.len(),
  }
}

#[hotpath::measure]
fn insert_multi_paragraph_fragment_at(document: &mut DocumentProjection, offset: DocumentOffset, fragment: &RichClipboardFragment) -> DocumentOffset {
  let Some(target) = document.paragraphs.get(offset.paragraph).cloned() else {
    return offset;
  };
  let byte = offset.byte.min(paragraph_text_len(&target));
  let paragraph_start = paragraph_byte_range(document, offset.paragraph).start;
  let (left_runs, right_runs) = split_runs_at(&target.runs, byte);
  let inserted_text = fragment
    .paragraphs
    .iter()
    .map(input_paragraph_text)
    .collect::<Vec<_>>()
    .join("\n");
  document.text.insert(paragraph_start + byte, &inserted_text);

  let last_ix = fragment.paragraphs.len() - 1;
  let mut replacements = Vec::with_capacity(fragment.paragraphs.len());
  for (ix, paragraph) in fragment.paragraphs.iter().enumerate() {
    let mut runs =
      Vec::with_capacity(paragraph.runs.len() + usize::from(ix == 0) * left_runs.len() + usize::from(ix == last_ix) * right_runs.len());
    if ix == 0 {
      runs.extend(left_runs.iter().cloned());
    }
    runs.extend(input_paragraph_text_runs(paragraph));
    if ix == last_ix {
      runs.extend(right_runs.iter().cloned());
    }
    replacements.push(Paragraph {
      style: if ix == 0 && !left_runs.is_empty() { target.style } else { paragraph.style },
      byte_range: 0..0,
      runs: merge_adjacent_runs(runs),
      version: target.version.wrapping_add(1),
    });
  }

  let replacement_count = replacements.len();
  {
    let paragraphs = paragraphs_mut(document);
    paragraphs.splice(offset.paragraph..=offset.paragraph, replacements)
  };
  for insert_ix in 1..replacement_count {
    insert_paragraph_id(document, offset.paragraph + insert_ix);
  }
  rebuild_document_offset_index(document);
  let block_replacements = document.paragraphs[offset.paragraph..offset.paragraph + replacement_count].to_vec();
  replace_paragraph_blocks(document, offset.paragraph, 1, &block_replacements);
  rebuild_document_sections(document);

  DocumentOffset {
    paragraph: offset.paragraph + last_ix,
    byte: input_paragraph_text(&fragment.paragraphs[last_ix]).len(),
  }
}

#[hotpath::measure]
fn input_paragraph_text(paragraph: &InputParagraph) -> String {
  paragraph.runs.iter().map(|run| run.text.as_str()).collect()
}

#[hotpath::measure]
fn input_paragraph_text_runs(paragraph: &InputParagraph) -> Vec<TextRun> {
  merge_adjacent_runs(
    paragraph
      .runs
      .iter()
      .filter(|run| !run.text.is_empty())
      .map(|run| TextRun {
        len: run.text.len(),
        styles: run.styles,
      })
      .collect(),
  )
}
