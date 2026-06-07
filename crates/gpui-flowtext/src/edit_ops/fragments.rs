#[hotpath::measure]
#[must_use]
pub fn selected_rich_fragment(document: &Document, range: Range<DocumentOffset>) -> RichClipboardFragment {
  if document.paragraphs.is_empty() || range.start.paragraph > range.end.paragraph {
    return RichClipboardFragment {
      format: RICH_TEXT_CLIPBOARD_FORMAT.to_owned(),
      paragraphs: Vec::new(),
      blocks: Vec::new(),
      assets: Vec::new(),
    };
  }
  let last_paragraph = document.paragraphs.len() - 1;
  let start_paragraph = range.start.paragraph.min(last_paragraph);
  let end_paragraph = range.end.paragraph.min(last_paragraph);
  if start_paragraph > end_paragraph {
    return RichClipboardFragment {
      format: RICH_TEXT_CLIPBOARD_FORMAT.to_owned(),
      paragraphs: Vec::new(),
      blocks: Vec::new(),
      assets: Vec::new(),
    };
  }
  let mut paragraphs = Vec::new();
  for paragraph_ix in start_paragraph..=end_paragraph {
    let paragraph = &document.paragraphs[paragraph_ix];
    let start = if paragraph_ix == range.start.paragraph {
      range.start.byte.min(paragraph_text_len(paragraph))
    } else {
      0
    };
    let end = if paragraph_ix == range.end.paragraph {
      range.end.byte.min(paragraph_text_len(paragraph))
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
