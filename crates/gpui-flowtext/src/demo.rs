use std::sync::Arc;

use crop::Rope;

use super::{
  AssetStore, DocumentParagraphInput, DocumentProjection, DocumentTheme, InputParagraph, InputRun, Paragraph, ParagraphOffsetIndex,
  ParagraphStyle, RunStyle, RunStyles, TextRun, document_ids_for_shape, merge_adjacent_runs, paragraph_blocks_from_paragraphs,
  rebuild_document_sections, reconcile_document_ids,
};

#[hotpath::measure]
#[must_use]
pub fn document_from_input(theme: DocumentTheme, paragraphs: Vec<InputParagraph>) -> DocumentProjection {
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
#[must_use]
pub fn document_from_paragraphs(theme: DocumentTheme, paragraphs: Vec<DocumentParagraphInput>) -> DocumentProjection {
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
fn document_from_stored_paragraphs(theme: DocumentTheme, text: String, mut stored_paragraphs: Vec<Paragraph>) -> DocumentProjection {
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
  let mut document = DocumentProjection {
    frontier: Vec::new(),
    text: Rope::from(text),
    paragraphs: Arc::new(stored_paragraphs),
    ids: document_ids_for_shape(paragraph_count, block_count),
    blocks: Arc::new(blocks),
    assets: AssetStore::default(),
    sections: Arc::new(Vec::new()),
    outline: Arc::new(Vec::new()),
    offset_index,
    theme,
  };
  reconcile_document_ids(&mut document);
  rebuild_document_sections(&mut document);
  document
}

#[hotpath::measure]
#[must_use]
pub fn blank_document() -> DocumentProjection {
  document_from_input(DocumentTheme::default(), Vec::new())
}

#[hotpath::measure]
#[must_use]
pub fn demo_document() -> DocumentProjection {
  let mut paragraphs = vec![
    InputParagraph {
      style: ParagraphStyle::Custom(0),
      runs: vec![plain("This is a Custom heading. It’s the highest-level heading in the document.")],
    },
    InputParagraph {
      style: ParagraphStyle::Custom(1),
      runs: vec![plain("This is a Custom subheading. It’s the second-highest level heading.")],
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
        run("highlighted spoken text", RunStyles::default().with(RunStyle::Highlight(1))),
        plain(", "),
        run("underlined words", RunStyles::default().with(RunStyle::Semantic(3))),
        plain(", and "),
        run("emphasis", RunStyles::default().with(RunStyle::Semantic(2))),
        plain(" while preserving the document as paragraph styles plus named run styles."),
      ],
    },
    InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![
        run(
          "This text is both underlined and highlighted, because it is all spoken. ",
          RunStyles::default()
            .with(RunStyle::Semantic(3))
            .with(RunStyle::Highlight(1)),
        ),
        run(
          "This text is both emphasized and highlighted, also all spoken.",
          RunStyles::default()
            .with(RunStyle::Semantic(2))
            .with(RunStyle::Highlight(1)),
        ),
      ],
    },
    InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![
        run(
          "This text is highlighted in the insert style. Can also be ",
          RunStyles::default().with(RunStyle::Highlight(2)),
        ),
        run(
          "underlined",
          RunStyles::default()
            .with(RunStyle::Highlight(2))
            .with(RunStyle::Semantic(3)),
        ),
        run(" and ", RunStyles::default().with(RunStyle::Highlight(2))),
        run(
          "emphasized.",
          RunStyles::default()
            .with(RunStyle::Highlight(2))
            .with(RunStyle::Semantic(2)),
        ),
      ],
    },
    InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![
        run(
          "Sometimes a team’s opponent will highlight in the same color as them, but you want to ‘rehighlight’ their card and read new portions. You need a different color to perform this operation. Here is a common one. Can also be ",
          RunStyles::default().with(RunStyle::Highlight(3)),
        ),
        run(
          "underlined",
          RunStyles::default()
            .with(RunStyle::Highlight(3))
            .with(RunStyle::Semantic(3)),
        ),
        run(" and ", RunStyles::default().with(RunStyle::Highlight(3))),
        run(
          "emphasized.",
          RunStyles::default()
            .with(RunStyle::Highlight(3))
            .with(RunStyle::Semantic(2)),
        ),
      ],
    },
    InputParagraph {
      style: ParagraphStyle::Custom(2),
      runs: vec![plain("This is a Block. It’s the third-highest level heading.")],
    },
    InputParagraph {
      style: ParagraphStyle::Custom(3),
      runs: vec![
        plain("This is a tag. It’s the fourth-highest level heading, tied with analytic."),
        plain(" Tags can also be "),
        run("underlined", RunStyles::default().with_direct_underline()),
        plain("."),
      ],
    },
    InputParagraph {
      style: ParagraphStyle::Custom(6),
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
        run("Codex 26", RunStyles::default().with(RunStyle::Semantic(1))),
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
        run("Usually ", RunStyles::default().with(RunStyle::Semantic(3))),
        run(
          "under a tag",
          RunStyles::default()
            .with(RunStyle::Semantic(3))
            .with(RunStyle::Highlight(1)),
        ),
        run(" and a cite line, ", RunStyles::default().with(RunStyle::Semantic(3))),
        run(
          "there’s",
          RunStyles::default()
            .with(RunStyle::Semantic(3))
            .with(RunStyle::Highlight(1)),
        ),
        run(" going to be the ", RunStyles::default().with(RunStyle::Semantic(3))),
        run(
          "content of the evidence.",
          RunStyles::default()
            .with(RunStyle::Semantic(3))
            .with(RunStyle::Highlight(1)),
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
            .with(RunStyle::Semantic(3))
            .with(RunStyle::Highlight(1)),
        ),
        run(
          "next tag",
          RunStyles::default()
            .with(RunStyle::Semantic(2))
            .with(RunStyle::Highlight(1)),
        ),
        run(
          " or ",
          RunStyles::default()
            .with(RunStyle::Semantic(3))
            .with(RunStyle::Highlight(1)),
        ),
        run(
          "analytic",
          RunStyles::default()
            .with(RunStyle::Semantic(2))
            .with(RunStyle::Highlight(1)),
        ),
        plain(", or sometimes the next heading if the evidence was the end of a section."),
      ],
    },
    InputParagraph {
      style: ParagraphStyle::Custom(4),
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
      0 => ParagraphStyle::Custom(3),
      3 => ParagraphStyle::Custom(6),
      6 => ParagraphStyle::Custom(4),
      _ => ParagraphStyle::Normal,
    };
    let spoken = RunStyles::default()
      .with(RunStyle::Semantic(3))
      .with(RunStyle::Highlight(1));
    let insert = RunStyles::default().with(RunStyle::Highlight(2));
    let alternative = RunStyles::default().with(RunStyle::Highlight(3));
    let emphasis = RunStyles::default().with(RunStyle::Semantic(2));

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
#[must_use]
pub fn plain(text: &str) -> InputRun {
  run(text, RunStyles::default())
}

#[hotpath::measure]
#[must_use]
pub fn run(text: &str, styles: RunStyles) -> InputRun {
  InputRun {
    text: text.to_owned(),
    styles,
  }
}
