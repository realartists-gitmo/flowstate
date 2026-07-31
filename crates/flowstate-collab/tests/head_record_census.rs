//! Census: which documents reach the projector with a FIRST PARAGRAPH that has
//! no durable metadata record?
//!
//! Those are the only documents whose head id would change if fabrication stops
//! minting the `paragraph.initial` / `block.body.initial` constants (fix "A" for
//! the boundary-0 identity bug). A head that already owns a durable record keeps
//! its id under every candidate fix, because records are honoured wherever their
//! cursor resolves.
//!
//! Run: `cargo test -p flowstate-collab --test head_record_census -- --nocapture --ignored`

#[cfg(test)]
mod tests {
  use flowstate_collab::crdt_runtime::CrdtRuntime;
  use flowstate_document::{
    DocumentProjection, InputParagraph, InputRun, ParagraphStyle, PARAGRAPHS_BY_ID, ROOT, ROOT_FIRST_PARAGRAPH_ID, RunStyles,
    document_from_input, DocumentTheme,
  };
  use loro::{LoroDoc, ValueOrContainer};

  /// Does the document carry a durable record under the boundary-0 constant?
  fn has_initial_record(doc: &LoroDoc) -> bool {
    let root = doc.get_map(ROOT);
    let Some(ValueOrContainer::Container(c)) = root.get(PARAGRAPHS_BY_ID) else {
      return false;
    };
    let Ok(paragraphs) = c.into_map() else { return false };
    paragraphs.get(ROOT_FIRST_PARAGRAPH_ID).is_some()
  }

  /// `loro_id_u128("paragraph.initial")`, verified against the real derivation.
  const INITIAL_ID: u128 = 260_163_308_421_898_818_295_378_351_936_162_206_376;

  /// A head id is FABRICATED-CONSTANT — the only case fix A changes — when the
  /// first paragraph projects the boundary-0 constant while no durable record is
  /// stored under that key. If a record exists (under any key, including
  /// `paragraph.<u128>` as the import path writes), the head's identity comes
  /// from that record and every candidate fix leaves it alone.
  fn report(label: &str, doc: &LoroDoc) -> bool {
    let projection = flowstate_document::document_from_loro(doc).expect("projection");
    let head = projection.ids.paragraph_ids.first().map(|id| id.0);
    let has_record = has_initial_record(doc);
    let fabricated_constant = head == Some(INITIAL_ID) && !has_record;
    println!(
      "  {:<44} head_id={:<42} initial_record={:<7} {}",
      label,
      head.map_or_else(|| "<none>".to_string(), |v| v.to_string()),
      if has_record { "yes" } else { "no" },
      if fabricated_constant { "*** A WOULD CHANGE THIS ***" } else { "unaffected" }
    );
    fabricated_constant
  }

  fn sample_projection() -> DocumentProjection {
    document_from_input(
      DocumentTheme::default(),
      vec![
        InputParagraph {
          style: ParagraphStyle::Normal,
          runs: vec![InputRun {
            text: "First paragraph".to_string(),
            styles: RunStyles::default(),
          }],
        },
        InputParagraph {
          style: ParagraphStyle::Normal,
          runs: vec![InputRun {
            text: "Second paragraph".to_string(),
            styles: RunStyles::default(),
          }],
        },
      ],
    )
  }

  #[test]
  #[ignore = "census/diagnostic, not an assertion"]
  fn census_of_head_records() {
    let mut affected = 0_usize;
    let mut total = 0_usize;

    println!("\n=== construction paths ===");
    let seeded = CrdtRuntime::new_empty("census").expect("seed");
    total += 1;
    if report("fresh seeded document (new_empty)", seeded.doc()) {
      affected += 1;
    }

    // The docx import path: parsed projection -> canonical Loro runtime.
    let imported = CrdtRuntime::from_document_projection(&sample_projection(), "census").expect("import");
    total += 1;
    if report("import path (from_document_projection)", imported.doc()) {
      affected += 1;
    }

    // The state the bug actually produces: split the head so a NEW paragraph
    // sits above the one owning the paragraph.initial record.
    {
      use flowstate_collab::local_write::{GateHolder, LocalDocHandle, LocalWriteConfig, SplitParagraphIntent, TextAnchor};
      let core = CrdtRuntime::new_empty("census-split").expect("runtime");
      let (handle, gate) = LocalDocHandle::new(core, LocalWriteConfig::default());
      let head = handle.projection().expect("projection").ids.paragraph_ids[0];
      handle
        .split_paragraph(SplitParagraphIntent {
          at: TextAnchor::new(head, 0),
          inherited_style: ParagraphStyle::Normal,
        })
        .expect("split head");
      let guard = gate.lock(GateHolder::ExportUpdates).expect("gate");
      total += 1;
      if report("after splitting the head (new para on top)", guard.doc()) {
        affected += 1;
      }
    }

    println!("\n=== real .db8 files on this machine ===");
    // `cargo test` runs with the PACKAGE dir as cwd, not the workspace root.
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
      .parent()
      .and_then(|p| p.parent())
      .expect("workspace root")
      .to_path_buf();
    let mut roots: Vec<std::path::PathBuf> = ["helpers/perf_fixtures", "helpers/demo", "Junk"]
      .iter()
      .map(|r| workspace.join(r))
      .collect();
    roots.retain(|r| r.is_dir());
    println!("  (scanning under {})", workspace.display());
    for root in roots {
      let Ok(entries) = std::fs::read_dir(&root) else { continue };
      for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("db8") {
          continue;
        }
        match CrdtRuntime::open_package(&path) {
          Ok(runtime) => {
            total += 1;
            if report(&path.display().to_string(), runtime.doc()) {
              affected += 1;
            }
          },
          Err(error) => println!("  {:<46} OPEN FAILED: {error:#}", path.display().to_string()),
        }
      }
    }

    println!("\n=== RESULT ===");
    println!("  documents inspected                : {total}");
    println!("  head id is a FABRICATED constant  : {affected}");
    println!("  (those are the only ids fix A would change)\n");
  }
}
