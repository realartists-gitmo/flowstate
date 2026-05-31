#[hotpath::measure]
fn insert_standalone_paragraphs_into_projection(
  document: &mut Document,
  insert_paragraph_ix: usize,
  inserted: &[InputParagraph],
) -> Vec<Paragraph> {
  if inserted.is_empty() {
    return Vec::new();
  }
  let mut entries = document
    .paragraphs
    .iter()
    .enumerate()
    .map(|(paragraph_ix, paragraph)| (paragraph.clone(), paragraph_text(document, paragraph_ix)))
    .collect::<Vec<_>>();
  let inserted_entries = inserted
    .iter()
    .map(|paragraph| {
      let text = input_paragraph_text(paragraph);
      (paragraph_from_input_paragraph(paragraph), text)
    })
    .collect::<Vec<_>>();
  let insert_ix = insert_paragraph_ix.min(entries.len());
  entries.splice(insert_ix..insert_ix, inserted_entries.clone());
  for relative_ix in 0..inserted.len() {
    insert_paragraph_id(document, insert_ix + relative_ix);
  }

  let mut text = String::new();
  let mut byte = 0;
  let mut paragraphs = Vec::with_capacity(entries.len());
  for (ix, (mut paragraph, paragraph_text)) in entries.into_iter().enumerate() {
    if ix > 0 {
      text.push('\n');
      byte += 1;
    }
    let start = byte;
    text.push_str(&paragraph_text);
    byte += paragraph_text.len();
    paragraph.byte_range = start..byte;
    paragraphs.push(paragraph);
  }
  let inserted_paragraphs = paragraphs[insert_ix..insert_ix + inserted.len()].to_vec();
  document.text = Rope::from(text);
  document.paragraphs = Arc::new(paragraphs);
  document.offset_index = ParagraphOffsetIndex::new(&document.paragraphs);
  rebuild_document_sections(document);
  inserted_paragraphs
}

