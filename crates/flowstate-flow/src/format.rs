//! The immutable flow FORMAT: the plain spreadsheet definition written once
//! into `flow.meta` at document creation (flow architecture spec Part 2.1)
//! and never mutated — every peer materializes against the same definition.
//! A spreadsheet has no sheet "types": every sheet is a plain grid whose
//! columns are seeded from the format's default column run (A, B, C, …) and
//! are then free-form (renamed, moved, resized, added, deleted).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type FormatId = Uuid;
pub type SheetId = Uuid;
pub type ColumnId = Uuid;
pub type RowId = Uuid;
pub type CellId = Uuid;
pub type StrokeId = Uuid;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnDefinition {
  pub id: ColumnId,
  pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowFormat {
  pub id: FormatId,
  pub name: String,
  /// The default column run a NEW sheet is seeded with (A, B, C, …). Copied
  /// into the sheet at creation; afterwards they are ordinary columns.
  pub default_columns: Vec<ColumnDefinition>,
}

impl FlowFormat {
  pub fn spreadsheet() -> Self {
    Self {
      id: Uuid::new_v4(),
      name: "Spreadsheet".into(),
      default_columns: ["A", "B", "C", "D", "E"]
        .iter()
        .map(|label| ColumnDefinition {
          id: Uuid::new_v4(),
          label: (*label).into(),
        })
        .collect(),
    }
  }
}
