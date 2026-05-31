use std::sync::Arc;

use crop::Rope;

use super::*;

#[hotpath::measure]
pub fn document_from_input(theme: DocumentTheme, paragraphs: Vec<InputParagraph>) -> Document {
  let text_capacity = paragraphs
    .iter()
    .map(|paragraph| {
      paragraph
        .runs
        .iter()
        .map(|run| run.text.len())
        .sum::<usize>()
    })
    .sum::<usize>()
    + paragraphs.len().saturating_sub(1);
  let mut text = String::with_capacity(text_capacity);
  let mut stored_paragraphs = Vec::with_capacity(paragraphs.len());
  for (ix, paragraph) in paragraphs.into_iter().enumerate() {
    if ix > 0 {
      text.push('\n');
    }
    let start = text.len();
    let mut runs = Vec::with_capacity(paragraph.runs.len());
    for run in paragraph.runs {
      let len = run.text.len();
      text.push_str(&run.text);
      runs.push(TextRun { len, styles: run.styles });
    }
    let end = text.len();
    stored_paragraphs.push(Paragraph {
      style: paragraph.style,
      byte_range: start..end,
      runs: merge_adjacent_runs(runs),
      version: 0,
    });
  }
  document_from_stored_paragraphs(theme, text, stored_paragraphs)
}

#[hotpath::measure]
pub fn document_from_paragraphs(theme: DocumentTheme, paragraphs: Vec<DocumentParagraphInput>) -> Document {
  let text_capacity = paragraphs
    .iter()
    .map(|paragraph| {
      paragraph
        .runs
        .iter()
        .map(|run| run.text.len())
        .sum::<usize>()
    })
    .sum::<usize>()
    + paragraphs.len().saturating_sub(1);
  let mut text = String::with_capacity(text_capacity);
  let mut stored_paragraphs = Vec::with_capacity(paragraphs.len());
  for (ix, paragraph) in paragraphs.into_iter().enumerate() {
    if ix > 0 {
      text.push('\n');
    }
    let start = text.len();
    let mut runs = Vec::with_capacity(paragraph.runs.len());
    for run in paragraph.runs {
      let len = run.text.len();
      text.push_str(&run.text);
      runs.push(TextRun { len, styles: run.styles });
    }
    let end = text.len();
    stored_paragraphs.push(Paragraph {
      style: paragraph.style,
      byte_range: start..end,
      runs: merge_adjacent_runs(runs),
      version: 0,
    });
  }
  document_from_stored_paragraphs(theme, text, stored_paragraphs)
}

#[hotpath::measure]
fn document_from_stored_paragraphs(theme: DocumentTheme, text: String, mut stored_paragraphs: Vec<Paragraph>) -> Document {
  if stored_paragraphs.is_empty() {
    stored_paragraphs.push(Paragraph {
      style: ParagraphStyle::Normal,
      byte_range: 0..0,
      runs: Vec::new(),
      version: 0,
    });
  }
  let offset_index = ParagraphOffsetIndex::new(&stored_paragraphs);
  let blocks = paragraph_blocks_from_paragraphs(&stored_paragraphs);
  let paragraph_count = stored_paragraphs.len();
  let block_count = blocks.len();
  let mut document = Document {
    text: Rope::from(text),
    paragraphs: Arc::new(stored_paragraphs),
    ids: document_ids_for_shape(paragraph_count, block_count),
    blocks: Arc::new(blocks),
    assets: AssetStore::default(),
    sections: Arc::new(Vec::new()),
    offset_index,
    theme,
  };
  reconcile_document_ids(&mut document);
  rebuild_document_sections(&mut document);
  document
}

#[hotpath::measure]
pub fn blank_document() -> Document {
  document_from_input(DocumentTheme::default(), Vec::new())
}

#[hotpath::measure]
pub fn demo_document() -> Document {
  let mut paragraphs = vec![
    InputParagraph {
      style: ParagraphStyle::Pocket,
      runs: vec![plain("This is a Pocket. It’s the highest-level heading in the document.")],
    },
    InputParagraph {
      style: ParagraphStyle::Hat,
      runs: vec![plain("This is a Hat. It’s the second-highest level heading.")],
    },
    InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain(
        "Visual parity matters because former Word users expect familiar line spacing, heading hierarchy, highlighting, and underline behavior.",
      )],
    },
    InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![
        plain("This paragraph mixes "),
        run("highlighted spoken text", RunStyles::default().with(RunStyle::HighlightSpoken)),
        plain(", "),
        run("underlined words", RunStyles::default().with(RunStyle::Underline)),
        plain(", and "),
        run("emphasis", RunStyles::default().with(RunStyle::Emphasis)),
        plain(" while preserving the document as paragraph styles plus named run styles."),
      ],
    },
    InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![
        run(
          "This text is both underlined and highlighted, because it is all spoken. ",
          RunStyles::default()
            .with(RunStyle::Underline)
            .with(RunStyle::HighlightSpoken),
        ),
        run(
          "This text is both emphasized and highlighted, also all spoken.",
          RunStyles::default()
            .with(RunStyle::Emphasis)
            .with(RunStyle::HighlightSpoken),
        ),
      ],
    },
    InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![
        run(
          "This text is highlighted in the insert style. Can also be ",
          RunStyles::default().with(RunStyle::HighlightInsert),
        ),
        run(
          "underlined",
          RunStyles::default()
            .with(RunStyle::HighlightInsert)
            .with(RunStyle::Underline),
        ),
        run(" and ", RunStyles::default().with(RunStyle::HighlightInsert)),
        run(
          "emphasized.",
          RunStyles::default()
            .with(RunStyle::HighlightInsert)
            .with(RunStyle::Emphasis),
        ),
      ],
    },
    InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![
        run(
          "Sometimes a team’s opponent will highlight in the same color as them, but you want to ‘rehighlight’ their card and read new portions. You need a different color to perform this operation. Here is a common one. Can also be ",
          RunStyles::default().with(RunStyle::HighlightAlternative),
        ),
        run(
          "underlined",
          RunStyles::default()
            .with(RunStyle::HighlightAlternative)
            .with(RunStyle::Underline),
        ),
        run(" and ", RunStyles::default().with(RunStyle::HighlightAlternative)),
        run(
          "emphasized.",
          RunStyles::default()
            .with(RunStyle::HighlightAlternative)
            .with(RunStyle::Emphasis),
        ),
      ],
    },
    InputParagraph {
      style: ParagraphStyle::Block,
      runs: vec![plain("This is a Block. It’s the third-highest level heading.")],
    },
    InputParagraph {
      style: ParagraphStyle::Tag,
      runs: vec![
        plain("This is a tag. It’s the fourth-highest level heading, tied with analytic."),
        plain(" Tags can also be "),
        run("underlined", RunStyles::default().with_direct_underline()),
        plain("."),
      ],
    },
    InputParagraph {
      style: ParagraphStyle::Undertag,
      runs: vec![
        plain(
          "This is an undertag. It usually goes below a tag and adds context that will be deleted when sent to opponents, and is usually never read. It can also be ",
        ),
        run("underlined", RunStyles::default().with_direct_underline()),
        plain("."),
      ],
    },
    InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![
        run("Codex 26", RunStyles::default().with(RunStyle::Cite)),
        plain(" is a wonderful coder! (This is a citation"),
        plain(
          ". It usually begins with the special cite text to denote the read portion, then transitions into more detailed normal text to denote the unread portion. The unread text can be ",
        ),
        run("underlined", RunStyles::default().with_direct_underline()),
        plain(", though it is uncommon, and in theory the cite text can be underlined too.)"),
      ],
    },
    InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![
        run("Usually ", RunStyles::default().with(RunStyle::Underline)),
        run(
          "under a tag",
          RunStyles::default()
            .with(RunStyle::Underline)
            .with(RunStyle::HighlightSpoken),
        ),
        run(" and a cite line, ", RunStyles::default().with(RunStyle::Underline)),
        run(
          "there’s",
          RunStyles::default()
            .with(RunStyle::Underline)
            .with(RunStyle::HighlightSpoken),
        ),
        run(" going to be the ", RunStyles::default().with(RunStyle::Underline)),
        run(
          "content of the evidence.",
          RunStyles::default()
            .with(RunStyle::Underline)
            .with(RunStyle::HighlightSpoken),
        ),
      ],
    },
    InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("It might continue for multiple paragraphs like this.")],
    },
    InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![
        plain("But then "),
        run(
          "it’ll end with the ",
          RunStyles::default()
            .with(RunStyle::Underline)
            .with(RunStyle::HighlightSpoken),
        ),
        run(
          "next tag",
          RunStyles::default()
            .with(RunStyle::Emphasis)
            .with(RunStyle::HighlightSpoken),
        ),
        run(
          " or ",
          RunStyles::default()
            .with(RunStyle::Underline)
            .with(RunStyle::HighlightSpoken),
        ),
        run(
          "analytic",
          RunStyles::default()
            .with(RunStyle::Emphasis)
            .with(RunStyle::HighlightSpoken),
        ),
        plain(", or sometimes the next heading if the evidence was the end of a section."),
      ],
    },
    InputParagraph {
      style: ParagraphStyle::Analytic,
      runs: vec![
        plain("This is an analytic"),
        plain(", a fourth-highest level heading, tied with tag."),
        plain(" "),
        plain("It’"),
        plain("s meant to be independent of evidence and deleted on the version of the document you send to your opponents. It can be "),
        run("underlined", RunStyles::default().with_direct_underline()),
        plain("."),
      ],
    },
  ];

  for ix in 1..=48 {
    let style = match ix % 9 {
      0 => ParagraphStyle::Tag,
      1 => ParagraphStyle::Normal,
      2 => ParagraphStyle::Normal,
      3 => ParagraphStyle::Undertag,
      4 => ParagraphStyle::Normal,
      5 => ParagraphStyle::Normal,
      6 => ParagraphStyle::Analytic,
      7 => ParagraphStyle::Normal,
      _ => ParagraphStyle::Normal,
    };
    let spoken = RunStyles::default()
      .with(RunStyle::Underline)
      .with(RunStyle::HighlightSpoken);
    let insert = RunStyles::default().with(RunStyle::HighlightInsert);
    let alternative = RunStyles::default().with(RunStyle::HighlightAlternative);
    let emphasis = RunStyles::default().with(RunStyle::Emphasis);

    paragraphs.push(InputParagraph {
      style,
      runs: vec![
        plain(&format!("Scrollable test paragraph {ix}. ")),
        run("This line is intentionally long enough to wrap at narrower window sizes, ", spoken),
        run(
          "so resizing the window should recompute layout width while preserving scrollable overflow. ",
          insert,
        ),
        run("It also keeps mixed rich-text spans active for paint and layout coverage. ", emphasis),
        run("Alternative highlight segment.", alternative),
      ],
    });
  }
  document_from_input(DocumentTheme::default(), paragraphs)
}

#[hotpath::measure]
pub fn plain(text: &str) -> InputRun {
  run(text, RunStyles::default())
}

#[hotpath::measure]
pub fn run(text: &str, styles: RunStyles) -> InputRun {
  InputRun {
    text: text.to_string(),
    styles,
  }
}
