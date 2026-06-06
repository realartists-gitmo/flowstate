#[hotpath::measure]
#[must_use]
pub fn selected_rich_fragment(document: &Document, range: Range<DocumentOffset>) -> RichClipboardFragment {
  let mut paragraphs = Vec::new();
  for paragraph_ix in range.start.paragraph..=range.end.paragraph {
    let paragraph = &document.paragraphs[paragraph_ix];
    let start = if paragraph_ix == range.start.paragraph { range.start.byte } else { 0 };
    let end = if paragraph_ix == range.end.paragraph {
      range.end.byte
    } else {
      paragraph_text_len(paragraph)
    };
    let mut runs = Vec::new();
    let mut offset = 0;
    for run in &paragraph.runs {
      let run_start = offset;
      let run_end = offset + run.len;
      offset = run_end;
      let clipped_start = run_start.max(start);
      let clipped_end = run_end.min(end);
      if clipped_start < clipped_end {
        let paragraph_range = paragraph_byte_range(document, paragraph_ix);
        runs.push(InputRun {
          text: document_text_slice(document, paragraph_range.start + clipped_start..paragraph_range.start + clipped_end),
          styles: run.styles,
        });
      }
    }
    paragraphs.push(InputParagraph {
      style: paragraph.style,
      runs,
    });
  }
  RichClipboardFragment {
    format: RICH_TEXT_CLIPBOARD_FORMAT.to_owned(),
    paragraphs,
    blocks: Vec::new(),
    assets: Vec::new(),
  }
}

