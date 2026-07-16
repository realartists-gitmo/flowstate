//! Generate rich Loro demo documents with a PLAUSIBLE history — for driving
//! the history tape/ledger/diff+blame by hand.
//!
//! Builds through REAL ops (never a one-shot import), so the replay timeline
//! actually tells a story:
//! - `history-demo.db8`: a debate case built over two "days" by two authors
//!   (Ada + Sol — blame resolves both), with tiered checkpoints in session
//!   bursts (autosave grain, session saves, named pins), comments (one left
//!   ORPHANED for the history-jump path), and a mid-history restore (so the
//!   "Before restore" safety pin shows on the tape). Revision timestamps are
//!   backdated onto a two-day schedule so the ledger's session grouping and
//!   the tape's time positions read plausibly.
//! - `history-demo.fl0`: a round being flowed — cells land in waves with
//!   named/session/auto checkpoints between them, so the flow tape has marks
//!   spread along the op timeline.
//!
//! Usage: `cargo run -p flowstate-corpus --bin history_fixture [-- <out-dir>]`
//! (default out-dir: Junk/)

use anyhow::{Context as _, Result};
use flowstate_collab::crdt_runtime::{CrdtRuntime, SemanticCommand};
use flowstate_collab::flow::{FlowDocHandle, FlowRuntime};
use flowstate_collab::local_write::GateHolder;
use flowstate_document::{
  DocumentPackage, PARAGRAPH_BLOCK, PARAGRAPH_HAT, PARAGRAPH_POCKET, PARAGRAPH_TAG, ParagraphStyle, RevisionKind, RevisionStamp,
  RunStyles, SEMANTIC_CITE,
};
use flowstate_document::{InputParagraph, InputRun};
use flowstate_flow::{CellPlacement, CellSeed, FlowIntent};
use gpui_flowtext::{DocumentOffset, EditorSelection};

/// End-append body writer over the runtime's `\n`-boundary body model
/// (body = `∂p0∂p1…`; paragraph i's boundary sits where it was split).
struct BodyWriter {
  /// Total body unicode length, including boundaries. A fresh doc is `\n`.
  len: usize,
  paragraphs: usize,
}

impl BodyWriter {
  fn new() -> Self {
    Self { len: 1, paragraphs: 1 }
  }

  /// Append a paragraph of `style` containing `text`; returns its index.
  fn para(&mut self, runtime: &mut CrdtRuntime, style: ParagraphStyle, text: &str) -> Result<usize> {
    if self.len == 1 && self.paragraphs == 1 {
      // The seed paragraph: style boundary 0, then fill it.
      runtime.command(SemanticCommand::SetParagraphStyle {
        boundary_unicode_index: 0,
        style,
      })?;
    } else {
      runtime.command(SemanticCommand::SplitParagraph {
        unicode_index: self.len,
        inherited_style: style,
      })?;
      self.len += 1;
      self.paragraphs += 1;
    }
    runtime.command(SemanticCommand::InsertText {
      unicode_index: self.len,
      text: text.to_string(),
      styles: RunStyles::default(),
    })?;
    self.len += text.chars().count();
    Ok(self.paragraphs - 1)
  }

  /// Append a tag paragraph whose trailing `cite` runs cite-styled.
  fn tag_with_cite(&mut self, runtime: &mut CrdtRuntime, tag: &str, cite: &str) -> Result<usize> {
    let paragraph = self.para(runtime, PARAGRAPH_TAG, tag)?;
    let cite_start = self.len + 1; // after the separating space below
    runtime.command(SemanticCommand::InsertText {
      unicode_index: self.len,
      text: format!(" {cite}"),
      styles: RunStyles::default(),
    })?;
    self.len += 1 + cite.chars().count();
    let cite_styles = RunStyles {
      semantic: SEMANTIC_CITE,
      ..RunStyles::default()
    };
    runtime.command(SemanticCommand::SetRunStyles {
      unicode_range: cite_start..self.len,
      styles: cite_styles,
    })?;
    Ok(paragraph)
  }
}

fn checkpoint(runtime: &mut CrdtRuntime, path: &std::path::Path, stamp: RevisionStamp) -> Result<()> {
  runtime
    .checkpoint_package("History Demo", Some(path.to_path_buf()), &stamp)
    .context("checkpointing the demo package")?;
  Ok(())
}

#[allow(clippy::too_many_lines, reason = "a linear story, told in order")]
fn build_db8(path: &std::path::Path) -> Result<()> {
  let mut ada = CrdtRuntime::new_empty("History Demo")?;
  ada.set_author_identity(7, Some("Ada".into()))?;
  let mut body = BodyWriter::new();

  // ---- Day 1, evening session: Ada builds the case shell ----
  body.para(&mut ada, PARAGRAPH_POCKET, "Warming Advantage")?;
  body.para(&mut ada, PARAGRAPH_HAT, "1AC — Advantage One")?;
  // Ada's first general comment registers her identity for blame.
  ada.create_comment(None, "Cut this file down before Friday's tournament", 7, "Ada")?;
  checkpoint(&mut ada, path, RevisionStamp::auto())?;

  body.para(&mut ada, PARAGRAPH_BLOCK, "Warming Real")?;
  body.tag_with_cite(
    &mut ada,
    "Anthropogenic warming is accelerating past every model consensus.",
    "Hansen '25",
  )?;
  body.para(
    &mut ada,
    ParagraphStyle::Normal,
    "New satellite altimetry confirms the upper-bound trajectory: sea surface temperatures have exceeded the CMIP6 envelope for nineteen consecutive months, and the rate of change itself is increasing.",
  )?;
  checkpoint(&mut ada, path, RevisionStamp::auto())?;

  body.para(&mut ada, PARAGRAPH_BLOCK, "Impact — Extinction")?;
  let doomed_tag = body.tag_with_cite(
    &mut ada,
    "Unchecked warming causes extinction — tipping cascades are irreversible.",
    "Xu & Ramanathan '24",
  )?;
  body.para(
    &mut ada,
    ParagraphStyle::Normal,
    "Beyond two degrees the cascade couples: permafrost carbon, AMOC slowdown, and ice-sheet dynamics stop being independent risks and become one system with no recovery path on any human timescale.",
  )?;
  checkpoint(&mut ada, path, RevisionStamp::session())?;
  let shell_done_frontier = ada.doc().state_frontiers().encode();
  ada.mint_named_pin_now("Case shell done")?;

  // A comment ANCHORED to the impact tag — this one will be orphaned later.
  let impact_tag_len = "Unchecked warming causes extinction — tipping cascades are irreversible."
    .chars()
    .count();
  let orphan_comment = ada.create_comment(
    Some(&EditorSelection::range(
      DocumentOffset {
        paragraph: doomed_tag,
        byte: 0,
      },
      DocumentOffset {
        paragraph: doomed_tag,
        byte: "Unchecked warming causes extinction".len(),
      },
    )),
    "This tag overclaims — soften before quarters",
    7,
    "Ada",
  )?;
  let _ = impact_tag_len;
  checkpoint(&mut ada, path, RevisionStamp::auto())?;

  // ---- Day 2, morning session: Sol cuts answers into the same file ----
  let mut sol = CrdtRuntime::from_doc(ada.doc().fork(), None, None)?;
  sol.set_author_identity(9, Some("Sol".into()))?;
  sol.create_comment(None, "Adding the AT: Adaptation block — flag anything that reads long", 9, "Sol")?;
  let mut sol_body = body;
  sol_body.para(&mut sol, PARAGRAPH_BLOCK, "AT: Adaptation Solves")?;
  sol_body.tag_with_cite(
    &mut sol,
    "Adaptation fails at scale — infrastructure lead times exceed the damage curve.",
    "Sovacool '26",
  )?;
  let sol_paragraph = sol_body.para(
    &mut sol,
    ParagraphStyle::Normal,
    "Every serious adaptation portfolio assumes stable financing across four decades; the damage function front-loads costs into the first one. The gap is not closable by markets pricing risk they systematically discount.",
  )?;
  let sol_update = sol
    .doc()
    .export(loro::ExportMode::all_updates())
    .map_err(|error| anyhow::anyhow!("exporting Sol's edits: {error}"))?;
  ada.import_remote_update(&sol_update)?;
  let body = sol_body;
  checkpoint(&mut ada, path, RevisionStamp::session())?;
  ada.mint_named_pin_now("After Sol's cut")?;
  checkpoint(&mut ada, path, RevisionStamp::auto())?;

  // ---- Day 2, afternoon session: the trim that orphans Ada's comment ----
  // Delete the impact tag's opening clause (the comment's anchor range).
  // Recompute the tag paragraph's body start: 1 + sum(len(p)+1 for p before).
  let projection = ada.projection_snapshot()?;
  let mut unicode_start = 1usize;
  for paragraph_ix in 0..doomed_tag {
    unicode_start += flowstate_document::paragraph_text(&projection, paragraph_ix).chars().count() + 1;
  }
  ada.command(SemanticCommand::DeleteRange {
    unicode_index: unicode_start,
    unicode_len: "Unchecked warming causes extinction".chars().count(),
  })?;
  ada.command(SemanticCommand::InsertText {
    unicode_index: unicode_start,
    text: "Warming beyond thresholds risks civilizational collapse".to_string(),
    styles: RunStyles::default(),
  })?;
  // Ada also trims the tail of SOL's paragraph — so the blame legend shows
  // both authors (removed-since spans are blamed by who WROTE the text).
  let projection = ada.projection_snapshot()?;
  let sol_text = flowstate_document::paragraph_text(&projection, sol_paragraph);
  let keep = "Every serious adaptation portfolio assumes stable financing across four decades;";
  let mut sol_start = 1usize;
  for paragraph_ix in 0..sol_paragraph {
    sol_start += flowstate_document::paragraph_text(&projection, paragraph_ix).chars().count() + 1;
  }
  ada.command(SemanticCommand::DeleteRange {
    unicode_index: sol_start + keep.chars().count(),
    unicode_len: sol_text.chars().count() - keep.chars().count(),
  })?;
  ada.command(SemanticCommand::InsertText {
    unicode_index: sol_start + keep.chars().count(),
    text: " the gap is structural, not a pricing failure.".to_string(),
    styles: RunStyles::default(),
  })?;
  checkpoint(&mut ada, path, RevisionStamp::auto())?;

  // A restore moment: Ada rewinds to the shell, then undoes the restore —
  // the "Before restore" named pin stays on the tape either way.
  ada.restore_frontier(&shell_done_frontier)?;
  ada.command(SemanticCommand::Undo)?;
  checkpoint(&mut ada, path, RevisionStamp::session())?;
  ada.mint_named_pin_now("Ready for round")?;
  checkpoint(&mut ada, path, RevisionStamp::auto())?;

  let _ = orphan_comment;

  // ---- Backdate the ledger onto a plausible two-day schedule ----
  // Records were minted in order; assign each a story timestamp. Base: two
  // days ago, 19:04.
  let now = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map_or(1_800_000_000, |elapsed| elapsed.as_secs() as i64);
  let day = 86_400;
  let base = now - 2 * day - (now % 3_600) + 4 * 60; // ~two days back, :04 past an hour
  // Offsets in minutes from `base`, one per record IN MINT ORDER (the initial
  // package snapshot is record 0). Bursts separated by >30min read as
  // sessions in the ledger.
  let day_minutes = 24 * 60;
  let schedule_minutes: &[i64] = &[
    0,    // day 1 evening: initial snapshot (first save)
    2,    // autosave
    9,    // autosave
    16,   // session save
    17,   // "Case shell done"
    24,   // autosave
    day_minutes + 13 * 60,      // day 2 morning: session save after Sol's import
    day_minutes + 13 * 60 + 1,  // "After Sol's cut"
    day_minutes + 13 * 60 + 8,  // autosave
    day_minutes + 18 * 60,      // day 2 afternoon: autosave after the trim
    day_minutes + 18 * 60 + 6,  // "Before restore" (the restore's safety pin)
    day_minutes + 18 * 60 + 7,  // session save
    day_minutes + 18 * 60 + 8,  // "Ready for round"
    day_minutes + 18 * 60 + 12, // final autosave
  ];
  let mut package = DocumentPackage::read(path).context("re-reading the demo package for backdating")?;
  for (index, revision) in package.revisions.iter_mut().enumerate() {
    let minutes = schedule_minutes
      .get(index)
      .copied()
      .unwrap_or_else(|| 19 * 60 + 12 + (index as i64 - schedule_minutes.len() as i64 + 1) * 3);
    revision.created_at_unix_secs = base + minutes * 60;
  }
  package.write(path).context("writing the backdated demo package")?;

  // Print the ledger so the story is checkable at a glance.
  println!("  ledger:");
  for revision in &package.revisions {
    let time = chrono_lite(revision.created_at_unix_secs);
    println!("    [{}] {:7} {}", time, format!("{:?}", revision.kind).to_lowercase(), revision.title);
  }
  let revision_count = package.revisions.len();
  println!(
    "db8: {} — {} paragraphs, {} revisions (named pins: {}), authors Ada + Sol, 1 orphaned comment",
    path.display(),
    body.paragraphs,
    revision_count,
    package
      .revisions
      .iter()
      .filter(|revision| revision.kind == RevisionKind::Named)
      .count(),
  );
  Ok(())
}

/// Day-relative HH:MM without pulling a date crate into the corpus.
fn chrono_lite(unix_secs: i64) -> String {
  let days = unix_secs / 86_400;
  let secs = unix_secs % 86_400;
  format!("d{} {:02}:{:02}", days % 100, secs / 3_600, (secs % 3_600) / 60)
}

fn flow_paragraphs(text: &str) -> Vec<InputParagraph> {
  vec![InputParagraph {
    style: PARAGRAPH_TAG,
    runs: vec![InputRun {
      text: text.into(),
      styles: RunStyles::default(),
    }],
  }]
}

fn build_fl0(path: &std::path::Path) -> Result<()> {
  let runtime = FlowRuntime::new_empty();
  let sheet_type = runtime.board().format.sheet_types[0].id;
  let (handle, _gate) = FlowDocHandle::new(runtime);
  let sheet = uuid::Uuid::new_v4();
  handle
    .apply(&FlowIntent::CreateSheet {
      sheet_id: sheet,
      name: "Round 3 vs Northwestern".into(),
      sheet_type_id: sheet_type,
    })
    .map_err(|error| anyhow::anyhow!("creating sheet: {error:?}"))?;

  let add = |column: usize, parent: Option<flowstate_flow::CellId>, text: &str| -> Result<flowstate_flow::CellId> {
    let cell_id = uuid::Uuid::new_v4();
    let placement = match parent {
      Some(parent) => CellPlacement::LastChildOf(parent),
      None => CellPlacement::SheetEnd { column_index: column },
    };
    handle
      .apply(&FlowIntent::AddCell {
        sheet_id: sheet,
        cell_id,
        placement,
        seed: CellSeed::Paragraphs(flow_paragraphs(text)),
      })
      .map_err(|error| anyhow::anyhow!("adding cell: {error:?}"))?;
    Ok(cell_id)
  };
  let pin = |title: Option<&str>, kind: RevisionKind| -> Result<()> {
    let mut guard = handle
      .gate()
      .lock(GateHolder::DocumentService)
      .map_err(|_| anyhow::anyhow!("flow gate poisoned"))?;
    guard.create_flow_checkpoint(title, kind)?;
    drop(guard);
    Ok(())
  };

  // Wave 1: the 1AC lands.
  let warming = add(0, None, "ADV 1: Warming — extinction via tipping cascades")?;
  let heg = add(0, None, "ADV 2: Hegemony — arctic posture collapse")?;
  let solvency = add(0, None, "Solvency — federal icebreaker fleet, 5 year window")?;
  pin(Some("1AC flowed"), RevisionKind::Named)?;

  // Wave 2: the 1NC answers arrive.
  let adaptation = add(1, Some(warming), "AT: Adaptation solves — tech curve outpaces damages")?;
  add(1, Some(warming), "AT: No cascade — tipping points overstated")?;
  let tradeoff = add(1, Some(heg), "DA: Shipbuilding tradeoff — links to sub production")?;
  add(1, Some(solvency), "AT: Delays — procurement never hits 5 years")?;
  pin(None, RevisionKind::Session)?;
  pin(Some("1NC in"), RevisionKind::Named)?;

  // Wave 3: 2AC extensions.
  add(2, Some(adaptation), "2AC: Adaptation fails at scale — Sovacool '26, financing gap")?;
  add(2, Some(tradeoff), "2AC: No link — different yards, different labor pools")?;
  pin(None, RevisionKind::Auto)?;
  add(2, Some(solvency), "2AC: Normal means solves delays — multi-year procurement authority")?;
  pin(Some("2AC extensions"), RevisionKind::Named)?;

  // Wave 4: the block.
  add(3, Some(adaptation), "2NC: Their ev is aspirational — no adaptation portfolio survives audit")?;
  pin(None, RevisionKind::Auto)?;

  let snapshot = {
    let guard = handle
      .gate()
      .lock(GateHolder::DocumentService)
      .map_err(|_| anyhow::anyhow!("flow gate poisoned"))?;
    guard.snapshot_bytes()?
  };
  flowstate_flow::persistence::save_snapshot_to(path, &snapshot).context("writing the demo flow")?;
  println!("fl0: {} — 1 sheet, 12 cells over 4 waves, 6 checkpoints (3 named)", path.display());
  Ok(())
}

fn main() -> Result<()> {
  let out_dir = std::env::args()
    .nth(1)
    .map_or_else(|| std::path::PathBuf::from("Junk"), std::path::PathBuf::from);
  std::fs::create_dir_all(&out_dir).context("creating the output directory")?;
  let db8_path = out_dir.join("history-demo.db8");
  let fl0_path = out_dir.join("history-demo.fl0");
  let _ = std::fs::remove_file(&db8_path);
  let _ = std::fs::remove_file(&fl0_path);
  build_db8(&db8_path)?;
  build_fl0(&fl0_path)?;
  verify(&db8_path, &fl0_path)?;
  println!("Open them in Flowstate and hit ctrl-alt-h (doc) or the flow history toggle.");
  Ok(())
}

/// Reload both artifacts the way the app would and prove the story holds:
/// the orphaned comment reads as orphaned with a jumpable birth frontier,
/// diff-vs-shell blames BOTH authors, and the flow tape has positioned marks.
fn verify(db8_path: &std::path::Path, fl0_path: &std::path::Path) -> Result<()> {
  let package = DocumentPackage::read(db8_path).context("reloading the demo package")?;
  let runtime = CrdtRuntime::from_package(package, None).context("opening the demo package as a runtime")?;
  let comments = runtime.comments();
  let orphan = comments
    .iter()
    .find(|thread| !thread.general && thread.anchor.is_none())
    .context("expected one orphaned comment")?;
  anyhow::ensure!(
    orphan.created_frontier.is_some(),
    "the orphan carries a birth frontier for history-jump"
  );
  let (historical, anchor) = runtime.frontier_comment_context(
    orphan.created_frontier.as_deref().expect("checked above"),
    orphan.comment_id,
  )?;
  anyhow::ensure!(anchor.is_some(), "the orphan's original anchor resolves at its birth frontier");
  anyhow::ensure!(!historical.paragraphs.is_empty(), "the historical view materializes");
  // Blame: diff the earliest named pin vs now and collect authors.
  let sol_pin = runtime
    .revisions()
    .into_iter()
    .find(|revision| revision.kind == RevisionKind::Named && revision.title == "After Sol's cut")
    .context("expected the Sol pin")?;
  let diff = runtime.frontier_diff_vs(&sol_pin.frontier, None)?;
  let authors: std::collections::HashSet<String> = diff
    .removed_since
    .iter()
    .filter_map(|span| span.author_display_name.clone())
    .collect();
  anyhow::ensure!(
    authors.contains("Ada") && authors.contains("Sol"),
    "the diff-vs-pin blames BOTH authors (got {authors:?})"
  );
  println!(
    "  verify db8: orphan jumpable ✓ · diff vs \"{}\": −{} +{} chars, blamed authors {:?}",
    sol_pin.title, diff.removed_chars, diff.inserted_chars, authors
  );

  let flow = flowstate_flow::persistence::load_flow_document(fl0_path).context("reloading the demo flow")?;
  anyhow::ensure!(!flow.projection().sheets.is_empty(), "the flow board materializes");
  let flow_runtime = FlowRuntime::from_snapshot(&flow.snapshot()?)?;
  let checkpoints = flow_runtime.flow_checkpoints();
  anyhow::ensure!(checkpoints.len() >= 6, "flow checkpoints survive the save");
  let frontiers: Vec<Vec<u8>> = checkpoints.iter().map(|checkpoint| checkpoint.frontier.clone()).collect();
  let positions = flow_runtime.history_timeline_positions(&frontiers)?;
  let resolved: Vec<f32> = positions.into_iter().flatten().collect();
  anyhow::ensure!(resolved.len() == checkpoints.len(), "every flow mark positions on the tape");
  anyhow::ensure!(
    resolved.windows(2).all(|pair| pair[0] <= pair[1]),
    "flow marks land in mint order along the tape"
  );
  println!(
    "  verify fl0: {} marks at tape positions {:?}",
    resolved.len(),
    resolved.iter().map(|position| (position * 100.0).round() / 100.0).collect::<Vec<_>>()
  );
  Ok(())
}
