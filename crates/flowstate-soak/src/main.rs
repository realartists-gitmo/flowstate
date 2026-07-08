//! Headless collaboration hotpath soak (field diagnosis, 2026-07-07).
//!
//! Reproduces the impact-doc slowness classes without a window or network — a
//! thin bin over the collab stack, so soak cycles never pay the app binary's
//! GPUI link:
//!
//! 1. **Local typing** — intents through the real `LocalDocHandle` write path
//!    plus the editor-side ordered-stream drain and patch apply (the exact
//!    projection work `sync_projection_from_authority` performs, minus GPU
//!    layout).
//! 2. **Paragraph splits, joins, cross-paragraph deletes** — the structural
//!    local-edit shapes.
//! 3. **Remote imports** — a second peer runtime types text and splits
//!    paragraphs; its update chunks import into the main runtime under the
//!    gate (the `doc_io` `import-remote-update` shape from the field logs),
//!    then drain into the simulated editor.
//!
//! Run with `--features hotpath-cpu` for the per-stage breakdown table that
//! prints on exit; the wall-clock distributions below print unconditionally.
//!
//! ```text
//! cargo run -p flowstate-soak --release --features hotpath-cpu -- <doc.docx> [--audit off|N]
//! ```

use std::{
  num::NonZeroU32,
  path::{Path, PathBuf},
  time::{Duration, Instant},
};

use clap::Parser;
use flowstate_collab::{
  crdt_runtime::CrdtRuntime,
  local_write::{
    DeleteRangeIntent, GateHolder, InsertTextIntent, JoinParagraphsIntent, LocalDocHandle, LocalWriteAuthority, LocalWriteConfig,
    SplitParagraphIntent, TextAnchor,
  },
};
use flowstate_document::{DocumentProjection, ParagraphId, ParagraphStyle, ProjectionStreamItem};

/// Headless collaboration hotpath soak over a real document.
#[derive(Parser)]
#[command(name = "flowstate-soak")]
struct Cli {
  /// Input `.docx` or package document.
  input: PathBuf,
  /// Local typing keystrokes to measure.
  #[arg(long, default_value_t = 160)]
  keystrokes: usize,
  /// Local paragraph splits to measure (joins/cross-deletes run at half this).
  #[arg(long, default_value_t = 8)]
  splits: usize,
  /// Remote import chunks to measure (every 6th is structural).
  #[arg(long, default_value_t = 24)]
  imports: usize,
  /// Release audit sampling: `off`, or audit every N-th intent
  /// (debug builds always audit every commit regardless).
  #[arg(long)]
  audit: Option<String>,
  /// Run the pure-Loro select-all micro-matrix (marks × subscription × undo)
  /// instead of the document soak.
  #[arg(long, default_value_t = false)]
  loro_micro: bool,
  /// Cap the select-all-restyle phase at N paragraphs (default: all).
  #[arg(long)]
  restyle_cap: Option<usize>,
}

/// Release-audit sampling requested on the CLI.
enum AuditSampling {
  ProfileDefault,
  Off,
  Every(NonZeroU32),
}

#[hotpath::main(functions_limit = 60)]
fn main() {
  let cli = Cli::parse();
  if cli.loro_micro {
    loro_micro();
    return;
  }
  let audit = cli.audit.as_deref().map_or(AuditSampling::ProfileDefault, |value| {
    if value.eq_ignore_ascii_case("off") {
      AuditSampling::Off
    } else {
      AuditSampling::Every(value.parse().expect("--audit takes `off` or a positive integer"))
    }
  });
  run(&cli.input, cli.keystrokes, cli.splits, cli.imports, &audit, cli.restyle_cap).expect("collab hotpath soak failed");
}

fn run(path: &Path, keystrokes: usize, splits: usize, imports: usize, audit: &AuditSampling, restyle_cap: Option<usize>) -> anyhow::Result<()> {
  let build_profile = if cfg!(debug_assertions) { "debug" } else { "release" };
  println!("collab-hotpath soak — build profile: {build_profile}");

  // ---- Load the document exactly the way the app open path does -------------
  let load_started = Instant::now();
  let runtime = load_runtime(path)?;
  println!("document load: {:?}", load_started.elapsed());

  let mut config = LocalWriteConfig::default();
  match audit {
    AuditSampling::ProfileDefault => {},
    AuditSampling::Off => config.release_audit_sample = None,
    AuditSampling::Every(sample) => config.release_audit_sample = Some(*sample),
  }
  println!(
    "release audit sampling: {:?} (ignored in debug builds — those audit every commit)",
    config.release_audit_sample
  );
  let (handle, gate) = LocalDocHandle::new(runtime, config);

  let projection = handle.projection().map_err(|error| anyhow::anyhow!("{error}"))?;
  let paragraph_count = projection.ids.paragraph_ids.len();
  let body_chars: usize = (0..projection.paragraphs.len())
    .map(|ix| flowstate_document::paragraph_text(&projection, ix).chars().count())
    .sum();
  println!("document shape: {paragraph_count} paragraphs, {body_chars} body chars, {} blocks", projection.blocks.len());

  // ---- Simulated editor: canonical attach + ordered-stream drains ------------
  let mut editor = LocalWriteAuthority::canonical_projection(&handle).map_err(|error| anyhow::anyhow!("{error}"))?;
  drain_into_editor(&handle, &mut editor)?;

  let target = mid_paragraph(&editor);

  // ---- Phase 1: local typing --------------------------------------------------
  let mut commit_times = Vec::with_capacity(keystrokes);
  let mut editor_apply_times = Vec::with_capacity(keystrokes);
  for i in 0..keystrokes {
    let started = Instant::now();
    handle
      .insert_text(InsertTextIntent {
        at: TextAnchor::new(target, usize::MAX),
        text: ((b'a' + (i % 26) as u8) as char).to_string(),
        style_override: None,
      })
      .map_err(|error| anyhow::anyhow!("typing intent rejected: {error}"))?;
    commit_times.push(started.elapsed());
    let apply_started = Instant::now();
    drain_into_editor(&handle, &mut editor)?;
    editor_apply_times.push(apply_started.elapsed());
  }
  summarize("local keystroke (intent commit, gate-held)", &mut commit_times);
  summarize("local keystroke (editor drain + patch apply)", &mut editor_apply_times);

  // ---- Phase 2: local paragraph splits ---------------------------------------
  let mut split_times = Vec::with_capacity(splits);
  for _ in 0..splits {
    let started = Instant::now();
    handle
      .split_paragraph(SplitParagraphIntent {
        at: TextAnchor::new(target, usize::MAX),
        inherited_style: ParagraphStyle::Normal,
      })
      .map_err(|error| anyhow::anyhow!("split intent rejected: {error}"))?;
    split_times.push(started.elapsed());
    drain_into_editor(&handle, &mut editor)?;
  }
  summarize("local split (intent commit, gate-held)", &mut split_times);

  // ---- Phase 2b: joins (Backspace at paragraph start) and cross-paragraph
  // deletes — the structural local edits from the field report.
  let mut join_times = Vec::new();
  for _ in 0..splits / 2 {
    let projection = handle.projection().map_err(|error| anyhow::anyhow!("{error}"))?;
    let Some(ix) = projection.ids.paragraph_ids.iter().position(|id| *id == target) else {
      break;
    };
    if ix + 1 >= projection.ids.paragraph_ids.len() {
      break;
    }
    let second = projection.ids.paragraph_ids[ix + 1];
    let started = Instant::now();
    handle
      .join_paragraphs(JoinParagraphsIntent { first: target, second })
      .map_err(|error| anyhow::anyhow!("join intent rejected: {error}"))?;
    join_times.push(started.elapsed());
    drain_into_editor(&handle, &mut editor)?;
  }
  summarize("local join (intent commit, gate-held)", &mut join_times);

  let mut cross_delete_times = Vec::new();
  for _ in 0..splits / 2 {
    // Create a fresh boundary, then delete across it.
    handle
      .split_paragraph(SplitParagraphIntent {
        at: TextAnchor::new(target, usize::MAX),
        inherited_style: ParagraphStyle::Normal,
      })
      .map_err(|error| anyhow::anyhow!("cross-delete setup split rejected: {error}"))?;
    drain_into_editor(&handle, &mut editor)?;
    let projection = handle.projection().map_err(|error| anyhow::anyhow!("{error}"))?;
    let Some(ix) = projection.ids.paragraph_ids.iter().position(|id| *id == target) else {
      break;
    };
    let Some(next) = projection.ids.paragraph_ids.get(ix + 1).copied() else {
      break;
    };
    let started = Instant::now();
    handle
      .delete_range(DeleteRangeIntent {
        start: TextAnchor::new(target, flowstate_document::paragraph_text(&projection, ix).len().saturating_sub(2)),
        end: TextAnchor::new(next, 0),
      })
      .map_err(|error| anyhow::anyhow!("cross-paragraph delete rejected: {error}"))?;
    cross_delete_times.push(started.elapsed());
    drain_into_editor(&handle, &mut editor)?;
  }
  summarize("local cross-paragraph delete (gate-held)", &mut cross_delete_times);

  // ---- Phase 3: remote imports -------------------------------------------------
  // A converged peer produced from the main doc's own snapshot, editing through
  // its own full write path; its update chunks import here like session_io does.
  let snapshot = {
    let guard = gate.lock(GateHolder::ExportUpdates).map_err(|_| anyhow::anyhow!("gate poisoned"))?;
    guard
      .doc()
      .export(loro::ExportMode::Snapshot)
      .map_err(|error| anyhow::anyhow!("snapshot export: {error}"))?
  };
  let peer_doc = loro::LoroDoc::new();
  peer_doc
    .import_with(&snapshot, "remote")
    .map_err(|error| anyhow::anyhow!("peer join import: {error}"))?;
  // Baseline BEFORE runtime startup: `from_doc` records replica metadata as
  // new commits, and the peer's typing ops causally depend on them — update
  // exports must include them or every main-side import reports pending
  // missing dependencies (and falls back to a full rebuild).
  let snapshot_vv = peer_doc.state_vv();
  let peer_runtime = CrdtRuntime::from_doc(peer_doc, None, None)?;
  let (peer_handle, peer_gate) = LocalDocHandle::new(
    peer_runtime,
    LocalWriteConfig {
      release_audit_sample: None,
    },
  );
  let peer_projection = peer_handle.projection().map_err(|error| anyhow::anyhow!("{error}"))?;
  let peer_target = mid_paragraph(&peer_projection);
  let mut peer_vv = snapshot_vv;

  let mut text_import_times = Vec::new();
  let mut structural_import_times = Vec::new();
  let mut import_apply_times = Vec::new();
  for i in 0..imports {
    // Realistic peer traffic: mostly short typing bursts, every 6th chunk is
    // structural (an Enter) — the field logs' import mix.
    let structural = i % 6 == 5;
    if structural {
      peer_handle
        .split_paragraph(SplitParagraphIntent {
          at: TextAnchor::new(peer_target, usize::MAX),
          inherited_style: ParagraphStyle::Normal,
        })
        .map_err(|error| anyhow::anyhow!("peer split rejected: {error}"))?;
    } else {
      for _ in 0..5 {
        peer_handle
          .insert_text(InsertTextIntent {
            at: TextAnchor::new(peer_target, usize::MAX),
            text: "r".to_string(),
            style_override: None,
          })
          .map_err(|error| anyhow::anyhow!("peer typing rejected: {error}"))?;
      }
    }
    let update = {
      let guard = peer_gate.lock(GateHolder::ExportUpdates).map_err(|_| anyhow::anyhow!("gate poisoned"))?;
      let update = guard
        .doc()
        .export(loro::ExportMode::updates(&peer_vv))
        .map_err(|error| anyhow::anyhow!("peer update export: {error}"))?;
      peer_vv = guard.doc().state_vv();
      update
    };
    let started = Instant::now();
    let mut guard = gate.lock(GateHolder::ImportChunk).map_err(|_| anyhow::anyhow!("gate poisoned"))?;
    let events = guard.import_remote_update(&update)?;
    drop(guard);
    for event in &events {
      if let flowstate_collab::crdt_runtime::RuntimeEvent::ProjectionUpdated { invalidation, .. } = event {
        println!(
          "  import {i} took the REBUILD path (structural={structural}, reason={:?})",
          invalidation.fallback_reason
        );
      }
    }
    if structural {
      structural_import_times.push(started.elapsed());
    } else {
      text_import_times.push(started.elapsed());
    }
    let apply_started = Instant::now();
    drain_into_editor(&handle, &mut editor)?;
    import_apply_times.push(apply_started.elapsed());
  }
  summarize("remote import (text chunk, gate-held)", &mut text_import_times);
  summarize("remote import (structural chunk, gate-held)", &mut structural_import_times);
  summarize("remote import (editor drain + apply)", &mut import_apply_times);

  // ---- Phase 3b: replace-all storm (find & replace). Every occurrence of a
  // common trigram across the whole doc, ONE compound intent — the §11
  // anti-amplification law under the heaviest realistic match count. Then one
  // undo (the storm is one undo member) to restore the text for phase 4.
  {
    let projection = handle.projection().map_err(|error| anyhow::anyhow!("{error}"))?;
    let mut matches = Vec::new();
    for (paragraph_ix, paragraph_id) in projection.ids.paragraph_ids.iter().enumerate() {
      let text = flowstate_document::paragraph_text(&projection, paragraph_ix);
      let mut from = 0usize;
      while let Some(found) = text[from..].find("the") {
        let byte = from + found;
        matches.push(flowstate_collab::local_write::ReplaceMatch {
          start: TextAnchor::new(*paragraph_id, byte),
          end: TextAnchor::new(*paragraph_id, byte + 3),
          styles: None,
        });
        from = byte + 3;
      }
    }
    let match_count = matches.len();
    if match_count > 0 {
      let started = Instant::now();
      handle
        .replace_matches(flowstate_collab::local_write::ReplaceMatchesIntent {
          matches,
          replacement: "thy".to_string(),
        })
        .map_err(|error| anyhow::anyhow!("replace-all storm rejected: {error}"))?;
      println!("replace-all storm ({match_count} matches, commit): {:?}", started.elapsed());
      let apply_started = Instant::now();
      drain_into_editor(&handle, &mut editor)?;
      println!("replace-all storm (editor drain + apply):        {:?}", apply_started.elapsed());
      let started = Instant::now();
      handle.apply_undo().map_err(|error| anyhow::anyhow!("undo replace-all rejected: {error}"))?;
      drain_into_editor(&handle, &mut editor)?;
      println!("undo replace-all storm (commit + drain):         {:?}", started.elapsed());
    } else {
      println!("replace-all storm: no matches found (skipped)");
    }
  }

  // ---- Phase 3c: select-all restyle (the 64.7s hotpath1 freeze). The editor
  // now sends ONE batched SetParagraphStyles intent for a selection-wide
  // restyle (§11 anti-amplification; the per-paragraph loop it replaced cost
  // one full write-path round trip per paragraph). Undo afterwards restores
  // styles and measures single-member undo of the mass restyle.
  {
    let projection = handle.projection().map_err(|error| anyhow::anyhow!("{error}"))?;
    let restyle_count = projection.ids.paragraph_ids.len().min(restyle_cap.unwrap_or(usize::MAX));
    if restyle_count == 0 {
      println!("select-all restyle: skipped (--restyle-cap 0)");
      // fall through to phase 4 with styles untouched
    } else {
    let paragraphs: Vec<_> = projection.ids.paragraph_ids.iter().copied().take(restyle_count).collect();
    let started = Instant::now();
    handle
      .set_paragraph_styles(flowstate_collab::local_write::SetParagraphStylesIntent {
        paragraphs,
        style: ParagraphStyle::Custom(1),
      })
      .map_err(|error| anyhow::anyhow!("batched restyle rejected: {error}"))?;
    println!("select-all restyle ({restyle_count} paragraphs, ONE batched commit): {:?}", started.elapsed());
    let apply_started = Instant::now();
    drain_into_editor(&handle, &mut editor)?;
    println!("select-all restyle (editor drain + apply):       {:?}", apply_started.elapsed());
    let started = Instant::now();
    handle.apply_undo().map_err(|error| anyhow::anyhow!("undo restyle rejected: {error}"))?;
    drain_into_editor(&handle, &mut editor)?;
    println!("undo select-all restyle (commit + drain):        {:?}", started.elapsed());
    let started = Instant::now();
    handle.apply_redo().map_err(|error| anyhow::anyhow!("redo restyle rejected: {error}"))?;
    drain_into_editor(&handle, &mut editor)?;
    println!("redo select-all restyle (commit + drain):        {:?}", started.elapsed());
    let started = Instant::now();
    handle.apply_undo().map_err(|error| anyhow::anyhow!("re-undo restyle rejected: {error}"))?;
    drain_into_editor(&handle, &mut editor)?;
    println!("re-undo select-all restyle (commit + drain):     {:?}", started.elapsed());
    }
  }

  // ---- Phase 4: select-all delete + retype (the ctrl-A field freeze). Runs
  // LAST because it guts the document. ------------------------------------------
  let projection = handle.projection().map_err(|error| anyhow::anyhow!("{error}"))?;
  let first = projection.ids.paragraph_ids[0];
  let last = *projection.ids.paragraph_ids.last().expect("paragraphs");
  let started = Instant::now();
  handle
    .delete_range(DeleteRangeIntent {
      start: TextAnchor::new(first, 0),
      end: TextAnchor::new(last, usize::MAX),
    })
    .map_err(|error| anyhow::anyhow!("select-all delete rejected: {error}"))?;
  println!("select-all delete (intent commit, gate-held):    {:?}", started.elapsed());
  let apply_started = Instant::now();
  drain_into_editor(&handle, &mut editor)?;
  println!("select-all delete (editor drain + apply):        {:?}", apply_started.elapsed());
  let started = Instant::now();
  let remaining = handle.projection().map_err(|error| anyhow::anyhow!("{error}"))?.ids.paragraph_ids[0];
  handle
    .insert_text(InsertTextIntent {
      at: TextAnchor::new(remaining, usize::MAX),
      text: "replacement text after select-all".to_string(),
      style_override: None,
    })
    .map_err(|error| anyhow::anyhow!("post-select-all retype rejected: {error}"))?;
  drain_into_editor(&handle, &mut editor)?;
  println!("select-all retype (commit + drain):              {:?}", started.elapsed());

  // ---- Phase 4b: undo/redo of the select-all (the ctrl-A + undo field
  // freeze): undo retype, undo the mass delete (restores the whole doc), redo.
  for label in ["undo retype", "undo select-all delete"] {
    let started = Instant::now();
    handle.apply_undo().map_err(|error| anyhow::anyhow!("{label} rejected: {error}"))?;
    drain_into_editor(&handle, &mut editor)?;
    println!("{label} (commit + drain):{:pad$}{:?}", "", started.elapsed(), pad = 31_usize.saturating_sub(label.len()));
  }
  let started = Instant::now();
  handle.apply_redo().map_err(|error| anyhow::anyhow!("redo select-all delete rejected: {error}"))?;
  drain_into_editor(&handle, &mut editor)?;
  println!("redo select-all delete (commit + drain):         {:?}", started.elapsed());

  // ---- Phase 4c: repeated mass undo/redo cycles. Registry hygiene proof:
  // every whole-doc restore fabricates a fresh record generation, and the
  // dead generations previously compounded the registry (field: scan P95
  // grew to seconds across a stress session). Non-increasing cycle times ⇒
  // the repair-pass prune is holding.
  for cycle in 0..2 {
    let started = Instant::now();
    handle.apply_undo().map_err(|error| anyhow::anyhow!("cycle undo rejected: {error}"))?;
    drain_into_editor(&handle, &mut editor)?;
    handle.apply_redo().map_err(|error| anyhow::anyhow!("cycle redo rejected: {error}"))?;
    drain_into_editor(&handle, &mut editor)?;
    println!("mass undo+redo cycle {cycle} (commit + drain):        {:?}", started.elapsed());
  }

  // ---- Phase 4d: PEER receipt of the whole-doc delete + restore history —
  // the 2026-07-07 field hang (peer froze importing the undo of a select-all
  // delete: thousands of dead-cursor history traces). Must complete in
  // seconds via the rebuild path, never trace per record.
  let peer_guard = peer_gate.lock(GateHolder::ExportUpdates).map_err(|_| anyhow::anyhow!("peer gate poisoned"))?;
  let peer_known_vv = peer_guard.doc().state_vv();
  drop(peer_guard);
  let main_guard = gate.lock(GateHolder::ExportUpdates).map_err(|_| anyhow::anyhow!("gate poisoned"))?;
  let mass_history_update = main_guard
    .doc()
    .export(loro::ExportMode::updates(&peer_known_vv))
    .map_err(|error| anyhow::anyhow!("mass-history export: {error}"))?;
  drop(main_guard);
  let mass_receipt_started = Instant::now();
  let mut peer_import_guard = peer_gate.lock(GateHolder::ImportChunk).map_err(|_| anyhow::anyhow!("peer gate poisoned"))?;
  peer_import_guard.import_remote_update(&mass_history_update)?;
  drop(peer_import_guard);
  println!(
    "peer receipt of mass delete/restore history ({} KB): {:?}",
    mass_history_update.len() / 1024,
    mass_receipt_started.elapsed()
  );

  // ---- Convergence proof: the soak measured real work, not dropped work -------
  let canonical = LocalWriteAuthority::canonical_projection(&handle).map_err(|error| anyhow::anyhow!("{error}"))?;
  anyhow::ensure!(editor.frontier == canonical.frontier, "simulated editor diverged from canonical frontier");
  println!("\nconverged: editor tracked all {} paragraphs at the canonical frontier", canonical.paragraphs.len());
  Ok(())
}

/// Pure-Loro bisect for the select-all-delete commit freeze: which combination
/// of {style marks, root subscription, `UndoManager`} makes `doc.commit()` of
/// a whole-body delete explode on a 2.6M-char text?
fn loro_micro() {
  let paragraph = "x".repeat(430);
  for marks in [false, true] {
    for with_subscription in [false, true] {
      for with_undo in [false, true] {
        let doc = loro::LoroDoc::new();
        let text = doc.get_text("t");
        for _ in 0..6000 {
          text.insert(text.len_unicode(), &paragraph).expect("insert");
          text.insert(text.len_unicode(), "\n").expect("insert boundary");
        }
        if marks {
          for i in 0..6000 {
            let start = i * 431;
            text.mark(start..start + 100, "bold", true).expect("mark");
          }
        }
        doc.commit();
        let _subscription = with_subscription.then(|| doc.subscribe_root(std::sync::Arc::new(|_event| {})));
        let _undo = with_undo.then(|| loro::UndoManager::new(&doc));
        let started = Instant::now();
        text.delete(0, text.len_unicode()).expect("select-all delete");
        let delete_elapsed = started.elapsed();
        let started = Instant::now();
        doc.commit();
        let commit_elapsed = started.elapsed();
        let undo_elapsed = _undo.map(|mut undo| {
          let started = Instant::now();
          let undone = undo.undo().expect("undo select-all delete");
          (started.elapsed(), undone)
        });
        println!(
          "marks={marks:<5} subscription={with_subscription:<5} undo={with_undo:<5} delete={delete_elapsed:>12.3?} commit={commit_elapsed:>12.3?} undo_op={undo_elapsed:?}"
        );
      }
    }
  }
}

fn load_runtime(path: &Path) -> anyhow::Result<CrdtRuntime> {
  let path = &scratch_input_copy(path)?;
  let is_docx = path
    .extension()
    .and_then(|extension| extension.to_str())
    .is_some_and(|extension| extension.eq_ignore_ascii_case("docx"));
  if is_docx {
    let (imported, _) = flowstate_docx::import_docx_to_loro(path, "Hotpath Soak")?;
    return CrdtRuntime::from_imported_document(imported);
  }
  CrdtRuntime::open_package(path)
}

/// The runtime PERSISTS update segments to the package path it was opened
/// from — running the soak directly against a user's .db8 appends every
/// synthetic edit into their document (learned the hard way on a field file).
/// Work on a scratch copy, always.
fn scratch_input_copy(path: &Path) -> anyhow::Result<std::path::PathBuf> {
  let scratch = std::env::temp_dir().join(format!(
    "flowstate-soak-{}-{}",
    std::process::id(),
    path.file_name().and_then(|name| name.to_str()).unwrap_or("input")
  ));
  std::fs::copy(path, &scratch)?;
  Ok(scratch)
}

fn mid_paragraph(projection: &DocumentProjection) -> ParagraphId {
  projection.ids.paragraph_ids[projection.ids.paragraph_ids.len() / 2]
}

fn drain_into_editor(handle: &LocalDocHandle, editor: &mut DocumentProjection) -> anyhow::Result<()> {
  for item in handle.drain_projection_stream().map_err(|error| anyhow::anyhow!("{error}"))? {
    match item {
      ProjectionStreamItem::Patches(batch) => {
        if std::env::var_os("SOAK_PATCH_DEBUG").is_some() {
          eprintln!("[batch] {} patches: {:?}", batch.patches.len(), batch.patches.iter().map(std::mem::discriminant).collect::<Vec<_>>());
        }
        if batch.new_frontier == editor.frontier {
          continue;
        }
        // Mirror `sync_projection_from_authority` exactly: in-place apply
        // (the batch apply is internally transactional) — so the measured
        // cost is the real editor-side cost.
        flowstate_document::apply_projection_patch_batch(editor, &batch)
          .map_err(|error| anyhow::anyhow!("ordered batch failed: {error:?}"))?;
      },
      ProjectionStreamItem::Replace(document) => *editor = *document,
    }
  }
  Ok(())
}

fn summarize(label: &str, times: &mut [Duration]) {
  if times.is_empty() {
    println!("{label:<48} (no samples)");
    return;
  }
  times.sort_unstable();
  let pick = |q: f64| times[((times.len() - 1) as f64 * q) as usize];
  println!(
    "{label:<48} n={:<4} p50={:>10.3?} p90={:>10.3?} p99={:>10.3?} max={:>10.3?}",
    times.len(),
    pick(0.50),
    pick(0.90),
    pick(0.99),
    times[times.len() - 1],
  );
}
