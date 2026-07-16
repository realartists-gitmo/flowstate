//! Synthetic perf fixtures for document shapes the real corpus does not cover.
//!
//! §perf-heaven T7: the 15k-document debate corpus has ZERO equation documents
//! (debate is pure carded text), so the equation projection / keystroke cost
//! (F17–F21) is entirely un-measured. These builders synthesize large equation-
//! and mixed-object-bearing documents so `collab_bench` / `hotpath_bench` can
//! measure them WITHOUT needing a real `.docx` — the doctrine's "build the
//! fixture first, measure, THEN cut; never optimize equations blind."
//!
//! They also give the object-bearing shape the object-free corpus never
//! triggers: a NON-empty `object_blocks_for_flow` candidate set (the §perf-heaven
//! T2/T4 paths) and `blocks.len() != paragraphs.len()`.

use flowstate_document::{
  AssetId, DocumentProjection, DocumentTheme, InputBlock, InputBlockAlignment, InputEquationBlock, InputEquationDisplay, InputEquationSyntax,
  InputImageBlock, InputImageSizing, InputParagraph, InputRun, ParagraphStyle, RunStyles, document_from_input_blocks,
};

fn paragraph(ix: usize) -> InputBlock {
  InputBlock::Paragraph(InputParagraph {
    style: if ix.is_multiple_of(40) {
      ParagraphStyle::Custom(2)
    } else {
      ParagraphStyle::Normal
    },
    runs: vec![InputRun {
      text: format!("Paragraph {ix}: carded evidence body with enough words to shape and reflow across the column."),
      styles: RunStyles::default(),
    }],
  })
}

fn equation(ix: usize) -> InputBlock {
  InputBlock::Equation(InputEquationBlock {
    source: format!("\\sum_{{i=0}}^{{{ix}}} x_i^2 = \\frac{{{ix}}}{{2}}\\pi + \\alpha_{{{ix}}}"),
    syntax: InputEquationSyntax::Latex,
    display: InputEquationDisplay::Display,
  })
}

fn image(ix: usize) -> InputBlock {
  InputBlock::Image(InputImageBlock {
    asset_id: AssetId(1),
    alt_text: format!("figure {ix}"),
    sizing: InputImageSizing::Intrinsic,
    alignment: InputBlockAlignment::Left,
    external_url: None,
  })
}

/// A large document with an equation block after every `equation_every`
/// paragraphs — the equation-heavy shape the corpus lacks. `equation_every == 0`
/// disables equations (degenerates to a plain-text doc).
#[must_use]
pub fn synthetic_equation_document(paragraphs: usize, equation_every: usize) -> DocumentProjection {
  let extra = if equation_every == 0 { 0 } else { paragraphs / equation_every.max(1) };
  let mut blocks = Vec::with_capacity(paragraphs + extra + 1);
  for ix in 0..paragraphs {
    blocks.push(paragraph(ix));
    if equation_every > 0 && ix % equation_every == equation_every - 1 {
      blocks.push(equation(ix));
    }
  }
  document_from_input_blocks(DocumentTheme::default(), blocks)
}

/// A large document interleaving equations, images, and text — the mixed-object
/// shape (a non-empty `object_blocks_for_flow` candidate set, the T4 path the
/// object-free corpus never exercises).
#[must_use]
pub fn synthetic_mixed_object_document(paragraphs: usize) -> DocumentProjection {
  let mut blocks = Vec::with_capacity(paragraphs + paragraphs / 4 + 1);
  for ix in 0..paragraphs {
    blocks.push(paragraph(ix));
    match ix % 12 {
      5 => blocks.push(equation(ix)),
      9 => blocks.push(image(ix)),
      _ => {},
    }
  }
  document_from_input_blocks(DocumentTheme::default(), blocks)
}

/// Parse a `synthetic:<kind>:<paragraphs>` selector into a projection and its
/// paragraph count, or return `None` if `selector` is not a synthetic spec (so
/// the caller falls through to a real `.docx` path). Kinds: `equations`
/// (default), `mixed`, `text`.
#[must_use]
pub fn from_selector(selector: &str) -> Option<(DocumentProjection, usize)> {
  let spec = selector.strip_prefix("synthetic:")?;
  let mut parts = spec.split(':');
  let kind = parts.next().unwrap_or("equations");
  let paragraphs: usize = parts
    .next()
    .and_then(|value| value.parse().ok())
    .unwrap_or(4000);
  let projection = match kind {
    "mixed" => synthetic_mixed_object_document(paragraphs),
    "text" => synthetic_equation_document(paragraphs, 0),
    _ => synthetic_equation_document(paragraphs, 8),
  };
  let count = projection.paragraphs.len();
  Some((projection, count))
}

#[cfg(test)]
mod tests {
  use super::*;
  use flowstate_collab::crdt_runtime::CrdtRuntime;
  use flowstate_document::{Block, document_from_loro_with_defects};

  fn equation_block_count(projection: &DocumentProjection) -> usize {
    projection
      .blocks
      .iter()
      .filter(|block| matches!(block, Block::Equation(_)))
      .count()
  }

  /// §perf-heaven T7 fidelity lock: the synthetic equation fixture must survive
  /// the COLD reopen condition (snapshot -> reimport into a fresh undecoded doc
  /// -> reproject) with NO DATA LOSS — every equation block round-trips. This
  /// closes the F17–F21 projection-fidelity measurement gap the object-free
  /// corpus leaves, and (because the reprojection lands on a `LazyLoad::Src`
  /// state) it also exercises the §perf-heaven T1 richtext fast-path equivalence
  /// assert in debug builds.
  ///
  /// The synthetic `document_from_input_blocks` construction anchors a standalone
  /// object block without minting the durable record for the boundary-less
  /// paragraph that follows it, so the cold projection reports REPAIRABLE
  /// `missing_paragraph_metadata`/`missing_paragraph_block` defects (the runtime
  /// repair pass fixes these canonically — this is exactly what the defect system
  /// is for). Those are tolerated; any CONTENT-affecting defect (orphan object,
  /// colliding anchor, table topology, invalid asset) is a hard failure.
  #[test]
  fn synthetic_equation_document_cold_projects_preserving_equations() {
    let projection = synthetic_equation_document(200, 8);
    let expected_equations = equation_block_count(&projection);
    assert!(expected_equations > 0, "fixture must contain equation blocks");

    let core = CrdtRuntime::from_document_projection(&projection, "eqn-fixture").expect("runtime");
    let snapshot = core
      .doc()
      .export(loro::ExportMode::Snapshot)
      .expect("snapshot");
    let cold = loro::LoroDoc::new();
    cold.import(&snapshot).expect("reimport");
    let (reprojected, defects) = document_from_loro_with_defects(&cold).expect("cold project");

    let non_repairable: Vec<_> = defects
      .iter()
      .filter(|defect| !matches!(defect.class(), "missing_paragraph_metadata" | "missing_paragraph_block"))
      .collect();
    assert!(
      non_repairable.is_empty(),
      "equation cold projection lost/corrupted content: {non_repairable:?}"
    );
    assert_eq!(
      equation_block_count(&reprojected),
      expected_equations,
      "equation blocks lost across cold projection",
    );
  }

  /// §perf-heaven T1: a large OBJECT-FREE doc is the case the fast path actually
  /// engages — `object_blocks_for_flow` early-returns (no `to_string`/`char_at`
  /// forcing `Dst`), and the O(1) `Src` `len_unicode` keeps the state lazy, so the
  /// cold projection's `to_delta` resolves the value from `Src`. In a debug build
  /// the equivalence assert fires on that path; this test cold-projects a pure-text
  /// doc and asserts text is preserved (and, in debug, that fast == full state).
  #[test]
  fn object_free_document_cold_projects_via_src_fastpath() {
    let projection = synthetic_equation_document(300, 0); // equation_every=0 → pure text
    assert_eq!(equation_block_count(&projection), 0, "must be object-free");
    let expected_text = projection.text.to_string();

    let core = CrdtRuntime::from_document_projection(&projection, "text-fixture").expect("runtime");
    let snapshot = core
      .doc()
      .export(loro::ExportMode::Snapshot)
      .expect("snapshot");
    let cold = loro::LoroDoc::new();
    cold.import(&snapshot).expect("reimport");
    let (reprojection, defects) = document_from_loro_with_defects(&cold).expect("cold project");

    assert!(defects.is_empty(), "object-free cold projection reported defects: {defects:?}");
    assert_eq!(
      reprojection.text.to_string(),
      expected_text,
      "cold projection text differs from the source"
    );
  }

  /// §perf-heaven T7.21 NET: an equation exports as its OWN `<w:p>` (`m:oMath`),
  /// so before the bare-object-wrapper collapse covered equations, reimporting
  /// that `<w:p>` minted a spurious empty paragraph (+1 `\n` boundary) PER
  /// equation. This round-trips the fixture through the REAL docx exporter +
  /// importer and asserts the equation count survives AND the paragraph count
  /// does not grow. Proven-trippable: reverting the equation half of the collapse
  /// (`has_objects` → images only) makes the paragraph count grow by the equation
  /// count and this fails.
  #[test]
  fn synthetic_equation_document_survives_docx_roundtrip() {
    let mut projection = synthetic_equation_document(60, 8);
    // The fixture builds with `DocumentTheme::default()`, which has no custom
    // paragraph-style slots; the exporter requires them (the fixture uses
    // `Custom(2)`). Use the real theme so export exercises the actual path.
    projection.theme = flowstate_document::flowstate_document_theme();
    let expected_equations = equation_block_count(&projection);
    let expected_paragraphs = projection.paragraphs.len();
    assert!(expected_equations > 0, "fixture must contain equations");

    let tmp = std::env::temp_dir().join(format!("fs-eqn-rt-{}.docx", std::process::id()));
    flowstate_docx::write_docx(&tmp, &projection).expect("write_docx");
    let bytes = std::fs::read(&tmp).expect("reread docx");
    let _ = std::fs::remove_file(&tmp);
    let (imported, _report) = flowstate_docx::import_docx_bytes_to_loro(&bytes, "eqn-rt").expect("reimport");
    let (reprojection, _defects) = document_from_loro_with_defects(&imported.doc).expect("reproject");

    assert_eq!(
      equation_block_count(&reprojection),
      expected_equations,
      "equation count changed across the docx round-trip",
    );
    assert_eq!(
      reprojection.paragraphs.len(),
      expected_paragraphs,
      "docx round-trip grew the paragraph count — a bare-equation-wrapper `<w:p>` minted a spurious boundary (T7.21 regression)",
    );
  }

  #[test]
  fn synthetic_mixed_object_document_carries_objects() {
    let projection = synthetic_mixed_object_document(120);
    assert!(
      projection.blocks.len() > projection.paragraphs.len(),
      "mixed fixture must carry non-paragraph objects (blocks={}, paragraphs={})",
      projection.blocks.len(),
      projection.paragraphs.len(),
    );
  }
}
