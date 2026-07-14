//! `FlowRuntime` write-path gates (build-order step 3): single-peer intents of
//! every class, no-op/rejection zero-mutation laws, undo/redo ping-pong per
//! class, undo-meta focus restore, and publish-queue convergence. Every
//! committed intent also runs the debug board audit (maintained board ==
//! fresh materialization), so these tests double as derivation-law checks.

#[cfg(test)]
mod tests {
  use std::sync::Arc;

  use flowstate_collab::flow::{FlowDocHandle, FlowPublishEvent, FlowRuntime};
  use flowstate_flow::format::{AnnotationOriginator, AnnotationStroke, BoardPoint, BoardRect, CellId, FlowFormat, SheetId, StrokeStyle};
  use flowstate_flow::intents::{CellPlacement, CellSeed, FlowDropIntent, FlowIntent, RelativePosition};
  use flowstate_flow::projection::FlowBoardProjection;
  use gpui_flowtext::local_intents::{
    DeleteRangeIntent, InsertTextIntent, JoinParagraphsIntent, LocalIntent, SetMarksIntent, SetParagraphStyleIntent, SplitParagraphIntent,
    TextAnchor,
  };
  use gpui_flowtext::{InputParagraph, InputRun, ParagraphStyle, RunStyles};
  use uuid::Uuid;

  struct Fixture {
    handle: Arc<FlowDocHandle>,
    format: FlowFormat,
    sheet: SheetId,
  }

  fn fixture() -> Fixture {
    let format = FlowFormat::policy_debate();
    let (handle, _gate) = FlowDocHandle::new(FlowRuntime::new(&format).expect("fresh flow runtime"));
    let sheet = Uuid::new_v4();
    handle
      .apply(FlowIntent::CreateSheet {
        sheet_id: sheet,
        name: "Case".into(),
        sheet_type_id: format.sheet_types[0].id,
      })
      .expect("create sheet");
    Fixture { handle, format, sheet }
  }

  impl Fixture {
    fn add_cell(&self, placement: CellPlacement) -> CellId {
      let cell = Uuid::new_v4();
      self
        .handle
        .apply(FlowIntent::AddCell {
          sheet_id: self.sheet,
          cell_id: cell,
          placement,
          seed: CellSeed::Empty,
        })
        .expect("add cell");
      cell
    }

    fn board(&self) -> FlowBoardProjection {
      self.handle.board_projection().expect("board projection")
    }

    fn cell_ids(&self) -> Vec<CellId> {
      self.board().sheets[0]
        .cells
        .iter()
        .map(|cell| cell.id)
        .collect()
    }

    fn paragraph_id(&self, cell: CellId, ix: usize) -> gpui_flowtext::ParagraphId {
      let projection = self.handle.open_cell(cell).expect("open cell");
      projection.ids.paragraph_ids[ix]
    }

    fn set_content(&self, cell: CellId, texts: &[(&str, ParagraphStyle)]) {
      let paragraphs = texts
        .iter()
        .map(|(text, style)| InputParagraph {
          style: *style,
          runs: vec![InputRun {
            text: (*text).to_string(),
            styles: RunStyles::default(),
          }],
        })
        .collect();
      self
        .handle
        .apply(FlowIntent::ReplaceCellContent {
          sheet_id: self.sheet,
          cell_id: cell,
          paragraphs,
        })
        .expect("replace cell content");
    }

    fn summary(&self, cell: CellId) -> String {
      self
        .board()
        .cell(cell)
        .map(|(_, cell)| cell.summary.summary_text.to_string())
        .expect("cell summary")
    }
  }

  fn stroke(sheet: SheetId, originator: &str) -> AnnotationStroke {
    AnnotationStroke {
      id: Uuid::new_v4(),
      sheet_id: sheet,
      originator: AnnotationOriginator(originator.into()),
      points: vec![BoardPoint { x: 0.0, y: 0.0 }, BoardPoint { x: 1.0, y: 1.0 }],
      style: StrokeStyle {
        color_rgba: 0xff00_00ff,
        width: 2.0,
        opacity: 0.5,
      },
      bbox: BoardRect {
        min: BoardPoint { x: 0.0, y: 0.0 },
        max: BoardPoint { x: 1.0, y: 1.0 },
      },
    }
  }

  #[test]
  fn structural_intents_maintain_the_board_in_place() {
    let fx = fixture();
    // Placements: end, top, sibling, first/last response.
    let first = fx.add_cell(CellPlacement::ColumnEnd { column_index: 0 });
    let top = fx.add_cell(CellPlacement::ColumnTop { column_index: 0 });
    let response = fx.add_cell(CellPlacement::ResponseTo { parent: first });
    let first_response = fx.add_cell(CellPlacement::FirstResponseTo { parent: first });
    let sibling = fx.add_cell(CellPlacement::Sibling {
      of: first,
      position: RelativePosition::After,
    });
    assert_eq!(fx.cell_ids(), vec![top, first, first_response, response, sibling]);
    let board = fx.board();
    assert_eq!(board.sheets[0].cell(response).unwrap().parent_id, Some(first));
    assert_eq!(board.sheets[0].cell(first_response).unwrap().parent_id, Some(first));
    assert_eq!(board.sheets[0].cell(sibling).unwrap().parent_id, None);

    // Subtree move: `first` (with its two responses) after `sibling`.
    fx.handle
      .apply(FlowIntent::MoveCellSubtree {
        sheet_id: fx.sheet,
        cell_id: first,
        drop: FlowDropIntent::AfterSibling(sibling),
      })
      .expect("move subtree");
    assert_eq!(fx.cell_ids(), vec![top, sibling, first, first_response, response]);

    // Rename + move sheet + second sheet.
    fx.handle
      .apply(FlowIntent::RenameSheet {
        sheet_id: fx.sheet,
        name: "Renamed".into(),
      })
      .expect("rename");
    let second_sheet = Uuid::new_v4();
    fx.handle
      .apply(FlowIntent::CreateSheet {
        sheet_id: second_sheet,
        name: "Other".into(),
        sheet_type_id: fx.format.sheet_types[1].id,
      })
      .expect("second sheet");
    fx.handle
      .apply(FlowIntent::MoveSheet {
        sheet_id: second_sheet,
        target_index: 0,
      })
      .expect("move sheet");
    let board = fx.board();
    assert_eq!(
      board
        .sheets
        .iter()
        .map(|sheet| sheet.id)
        .collect::<Vec<_>>(),
      vec![second_sheet, fx.sheet]
    );
    assert_eq!(board.sheet(fx.sheet).unwrap().name, "Renamed");

    // Delete a parent: children orphan canonically (no defects on rebuild).
    fx.handle
      .apply(FlowIntent::DeleteCell {
        sheet_id: fx.sheet,
        cell_id: first,
      })
      .expect("delete cell");
    let board = fx.board();
    assert_eq!(
      board
        .sheet(fx.sheet)
        .unwrap()
        .cell(response)
        .unwrap()
        .parent_id,
      None
    );
    assert_eq!(
      board
        .sheet(fx.sheet)
        .unwrap()
        .cell(first_response)
        .unwrap()
        .parent_id,
      None
    );

    fx.handle
      .apply(FlowIntent::DeleteSheet { sheet_id: second_sheet })
      .expect("delete sheet");
    assert_eq!(fx.board().sheets.len(), 1);
  }

  #[test]
  fn cell_text_intents_drive_the_cell_stream_and_summary() {
    let fx = fixture();
    let cell = fx.add_cell(CellPlacement::ColumnEnd { column_index: 0 });
    let authority = fx.handle.cell_authority(cell);
    let projection = fx.handle.open_cell(cell).expect("open cell");
    assert_eq!(projection.paragraphs.len(), 1);
    let paragraph = projection.ids.paragraph_ids[0];

    use gpui_flowtext::local_intents::LocalWriteAuthority as _;
    // Typing.
    let outcome = authority
      .apply(LocalIntent::InsertText(InsertTextIntent {
        at: TextAnchor::new(paragraph, 0),
        text: "Warming oceans".into(),
        style_override: None,
      }))
      .expect("insert text");
    let commit = outcome.commit();
    assert!(commit.selection_after.is_some(), "caret snapshot rides the outcome");
    let items = authority.drain_projection_stream().expect("drain");
    assert!(!items.is_empty(), "whole-cell Replace queued for the editor");
    assert_eq!(fx.summary(cell), "Warming oceans", "board summary refreshed");

    // Split + style + marks.
    authority
      .apply(LocalIntent::SplitParagraph(SplitParagraphIntent {
        at: TextAnchor::new(paragraph, 7),
        inherited_style: ParagraphStyle::Normal,
      }))
      .expect("split");
    let projection = authority.canonical_projection().expect("projection");
    assert_eq!(projection.paragraphs.len(), 2);
    let second = projection.ids.paragraph_ids[1];
    authority
      .apply(LocalIntent::SetParagraphStyle(SetParagraphStyleIntent {
        paragraph: second,
        style: flowstate_document::PARAGRAPH_ANALYTIC,
      }))
      .expect("style");
    authority
      .apply(LocalIntent::SetMarks(SetMarksIntent {
        start: TextAnchor::new(paragraph, 0),
        end: TextAnchor::new(paragraph, 4),
        styles: RunStyles {
          strikethrough: true,
          ..RunStyles::default()
        },
      }))
      .expect("marks");
    let projection = authority.canonical_projection().expect("projection");
    assert!(
      projection.paragraphs[0]
        .runs
        .iter()
        .any(|run| run.styles.strikethrough)
    );
    assert_eq!(projection.paragraphs[1].style, flowstate_document::PARAGRAPH_ANALYTIC);

    // Join back + delete a range.
    authority
      .apply(LocalIntent::JoinParagraphs(JoinParagraphsIntent { first: paragraph, second }))
      .expect("join");
    let projection = authority.canonical_projection().expect("projection");
    assert_eq!(projection.paragraphs.len(), 1);
    authority
      .apply(LocalIntent::DeleteRange(DeleteRangeIntent {
        start: TextAnchor::new(paragraph, 0),
        end: TextAnchor::new(paragraph, 8),
      }))
      .expect("delete range");
    assert_eq!(fx.summary(cell), "oceans");
  }

  #[test]
  fn strike_and_editable_and_annotations() {
    let fx = fixture();
    let cell = fx.add_cell(CellPlacement::ColumnEnd { column_index: 0 });
    fx.set_content(cell, &[("argument", ParagraphStyle::Normal)]);
    assert!(
      !fx
        .board()
        .cell(cell)
        .unwrap()
        .1
        .summary
        .uses_summary_projection
    );

    // Strike on, idempotent no-op, strike off.
    fx.handle
      .apply(FlowIntent::SetCellStruck {
        sheet_id: fx.sheet,
        cell_id: cell,
        struck: true,
      })
      .expect("strike");
    assert!(fx.board().cell(cell).unwrap().1.summary.struck);
    let outcome = fx
      .handle
      .apply(FlowIntent::SetCellStruck {
        sheet_id: fx.sheet,
        cell_id: cell,
        struck: true,
      })
      .expect("strike no-op");
    assert!(!outcome.changed, "same-state strike resolves to a no-op");
    fx.handle
      .apply(FlowIntent::SetCellStruck {
        sheet_id: fx.sheet,
        cell_id: cell,
        struck: false,
      })
      .expect("unstrike");
    assert!(!fx.board().cell(cell).unwrap().1.summary.struck);

    // EnsureCellEditable restyles the first paragraph to the tag style.
    fx.handle
      .apply(FlowIntent::EnsureCellEditable {
        sheet_id: fx.sheet,
        cell_id: cell,
      })
      .expect("ensure editable");
    assert!(
      fx.board()
        .cell(cell)
        .unwrap()
        .1
        .summary
        .uses_summary_projection
    );

    // Annotations: add two originators, clear one, delete respects ownership.
    let mine = stroke(fx.sheet, "me");
    let theirs = stroke(fx.sheet, "them");
    for annotation in [&mine, &theirs] {
      fx.handle
        .apply(FlowIntent::AddAnnotation {
          sheet_id: fx.sheet,
          stroke: annotation.clone(),
        })
        .expect("add annotation");
    }
    let refused = fx
      .handle
      .apply(FlowIntent::DeleteAnnotation {
        sheet_id: fx.sheet,
        stroke_id: theirs.id,
        originator: AnnotationOriginator("me".into()),
      })
      .expect("delete annotation (foreign)");
    assert!(!refused.changed, "cannot delete another originator's stroke");
    fx.handle
      .apply(FlowIntent::ClearAnnotations {
        sheet_id: Some(fx.sheet),
        originator: AnnotationOriginator("me".into()),
      })
      .expect("clear annotations");
    let board = fx.board();
    assert_eq!(board.sheets[0].annotations.len(), 1);
    assert_eq!(board.sheets[0].annotations[0].originator, AnnotationOriginator("them".into()));
  }

  #[test]
  fn rejections_and_no_ops_leave_the_frontier_untouched() {
    let fx = fixture();
    let cell = fx.add_cell(CellPlacement::ColumnEnd { column_index: 0 });
    let frontier = fx.handle.board_projection().expect("board");
    let before = fx
      .handle
      .apply(FlowIntent::ClearAnnotations {
        sheet_id: None,
        originator: AnnotationOriginator("nobody".into()),
      })
      .expect("clear nothing");
    assert!(!before.changed);

    // Unknown sheet/cell reject.
    assert!(
      fx.handle
        .apply(FlowIntent::RenameSheet {
          sheet_id: Uuid::new_v4(),
          name: "ghost".into(),
        })
        .is_err()
    );
    assert!(
      fx.handle
        .apply(FlowIntent::DeleteCell {
          sheet_id: fx.sheet,
          cell_id: Uuid::new_v4(),
        })
        .is_err()
    );
    // Self-drop move rejects.
    assert!(
      fx.handle
        .apply(FlowIntent::MoveCellSubtree {
          sheet_id: fx.sheet,
          cell_id: cell,
          drop: FlowDropIntent::LastChildOf(cell),
        })
        .is_err()
    );
    assert_eq!(fx.handle.board_projection().expect("board"), frontier, "no mutation escaped");
  }

  #[test]
  fn undo_redo_ping_pong_across_intent_classes() {
    let fx = fixture();
    let cell = fx.add_cell(CellPlacement::ColumnEnd { column_index: 0 });
    fx.set_content(cell, &[("tag line", flowstate_document::PARAGRAPH_TAG)]);
    let other = fx.add_cell(CellPlacement::ColumnEnd { column_index: 0 });

    // A representative mutation per class, applied in sequence.
    let steps: Vec<FlowIntent> = vec![
      FlowIntent::RenameSheet {
        sheet_id: fx.sheet,
        name: "Undoable".into(),
      },
      FlowIntent::MoveCellSubtree {
        sheet_id: fx.sheet,
        cell_id: other,
        drop: FlowDropIntent::BeforeSibling(cell),
      },
      FlowIntent::SetCellStruck {
        sheet_id: fx.sheet,
        cell_id: cell,
        struck: true,
      },
      FlowIntent::AddAnnotation {
        sheet_id: fx.sheet,
        stroke: stroke(fx.sheet, "me"),
      },
      FlowIntent::DeleteCell {
        sheet_id: fx.sheet,
        cell_id: other,
      },
      FlowIntent::CellText {
        cell_id: cell,
        intent: LocalIntent::InsertText(InsertTextIntent {
          at: TextAnchor::new(fx.paragraph_id(cell, 0), 0),
          text: "typed ".into(),
          style_override: None,
        }),
      },
    ];

    let mut boards = vec![fx.board()];
    for step in steps {
      fx.handle.apply(step).expect("apply step");
      boards.push(fx.board());
    }

    // Undo all the way down the stack, checking each intermediate board.
    for expected in boards.iter().rev().skip(1) {
      let outcome = fx.handle.undo().expect("undo");
      assert!(outcome.applied, "undo stack must not run dry early");
      assert_eq!(&fx.board(), expected, "undo restored the exact prior board");
    }
    // Redo all the way back up.
    for expected in boards.iter().skip(1) {
      let outcome = fx.handle.redo().expect("redo");
      assert!(outcome.applied, "redo stack must not run dry early");
      assert_eq!(&fx.board(), expected, "redo restored the exact next board");
    }
  }

  #[test]
  fn undo_meta_restores_board_focus_context() {
    let fx = fixture();
    let cell = fx.add_cell(CellPlacement::ColumnEnd { column_index: 0 });
    fx.handle
      .set_undo_context(flowstate_collab::flow::runtime::FlowUndoMeta {
        active_sheet: Some(fx.sheet),
        focused_cell: Some(cell),
        head_cursor: None,
        anchor_cursor: None,
      });
    fx.handle
      .apply(FlowIntent::SetCellStruck {
        sheet_id: fx.sheet,
        cell_id: cell,
        struck: true,
      })
      .expect("strike under context");
    fx.set_content(cell, &[("x", ParagraphStyle::Normal)]); // later commit with same context
    let outcome = fx.handle.undo().expect("undo");
    assert!(outcome.applied);
    let meta = outcome.meta.expect("undo meta restored");
    assert_eq!(meta.active_sheet, Some(fx.sheet));
    assert_eq!(meta.focused_cell, Some(cell));
  }

  #[test]
  fn publish_queue_converges_a_second_runtime() {
    let fx = fixture();
    let cell = fx.add_cell(CellPlacement::ColumnEnd { column_index: 0 });
    fx.set_content(cell, &[("shared", flowstate_document::PARAGRAPH_TAG)]);

    // Bootstrap peer B from a snapshot, then apply more local edits on A.
    let snapshot = fx
      .handle
      .with_test_runtime(|runtime| runtime.snapshot().expect("snapshot"));
    let (peer_b, _gate_b) = FlowDocHandle::new(FlowRuntime::from_snapshot(&snapshot).expect("peer B"));
    fx.handle
      .apply(FlowIntent::RenameSheet {
        sheet_id: fx.sheet,
        name: "Post-snapshot".into(),
      })
      .expect("rename");

    // Drain A's publish queue and import the update bytes into B.
    let events = fx
      .handle
      .with_test_runtime(FlowRuntime::take_pending_publish);
    let mut imported = false;
    for event in events {
      if let FlowPublishEvent::LocalUpdate { bytes, .. } = event {
        peer_b.with_test_runtime(|runtime| {
          runtime
            .import_remote_updates(&[bytes.as_slice()])
            .expect("import");
        });
        imported = true;
      }
    }
    assert!(imported, "local commits queued publishable updates");
    assert_eq!(peer_b.board_projection().expect("board"), fx.board(), "peers converged");
  }
}
