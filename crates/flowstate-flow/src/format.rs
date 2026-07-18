//! The immutable flow FORMAT: which sheets a flow can contain and which
//! speech columns each sheet type carries. Written once into `flow.meta` at
//! document creation (flow architecture spec Part 2.1) and never mutated —
//! every peer materializes against the same definition.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type FormatId = Uuid;
pub type SheetTypeId = Uuid;
pub type SheetId = Uuid;
pub type ColumnId = Uuid;
pub type RowId = Uuid;
pub type CellId = Uuid;
pub type StrokeId = Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArgumentSide {
  One,
  Two,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnDefinition {
  pub id: ColumnId,
  pub label: String,
  pub side: ArgumentSide,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SheetTypeDefinition {
  pub id: SheetTypeId,
  pub name: String,
  pub columns: Vec<ColumnDefinition>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowFormat {
  pub id: FormatId,
  pub name: String,
  pub sheet_types: Vec<SheetTypeDefinition>,
}

impl FlowFormat {
  pub fn policy_debate() -> Self {
    let affirmative = sheet_type(
      "Affirmative",
      &[
        ("1AC", ArgumentSide::One),
        ("1NC", ArgumentSide::Two),
        ("2AC", ArgumentSide::One),
        ("Block", ArgumentSide::Two),
        ("1AR", ArgumentSide::One),
        ("2NR", ArgumentSide::Two),
        ("2AR", ArgumentSide::One),
      ],
    );
    let negative = sheet_type(
      "Negative",
      &[
        ("1NC", ArgumentSide::Two),
        ("2AC", ArgumentSide::One),
        ("Block", ArgumentSide::Two),
        ("1AR", ArgumentSide::One),
        ("2NR", ArgumentSide::Two),
        ("2AR", ArgumentSide::One),
      ],
    );
    Self {
      id: Uuid::new_v4(),
      name: "Policy Debate".into(),
      sheet_types: vec![affirmative, negative],
    }
  }

  pub fn sheet_type(&self, id: SheetTypeId) -> Option<&SheetTypeDefinition> {
    self
      .sheet_types
      .iter()
      .find(|definition| definition.id == id)
  }
}

fn sheet_type(name: &str, columns: &[(&str, ArgumentSide)]) -> SheetTypeDefinition {
  SheetTypeDefinition {
    id: Uuid::new_v4(),
    name: name.into(),
    columns: columns
      .iter()
      .map(|(label, side)| ColumnDefinition {
        id: Uuid::new_v4(),
        label: (*label).into(),
        side: *side,
      })
      .collect(),
  }
}
