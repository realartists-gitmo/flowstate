//! The immutable flow FORMAT vocabulary (.fl0 v2 spec, Part A) plus the shared
//! id aliases and annotation data types. The format is written once into
//! `flow.meta` as a postcard blob and never mutated afterwards — every peer
//! reads the identical column/sheet-type topology, so the normalization law can
//! treat it as a constant.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type FormatId = Uuid;
pub type SheetTypeId = Uuid;
pub type SheetId = Uuid;
pub type ColumnId = Uuid;
pub type CellId = Uuid;
pub type StrokeId = Uuid;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AnnotationOriginator(pub String);

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

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BoardPoint {
  pub x: f32,
  pub y: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BoardRect {
  pub min: BoardPoint,
  pub max: BoardPoint,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrokeStyle {
  pub color_rgba: u32,
  pub width: f32,
  pub opacity: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnnotationStroke {
  pub id: StrokeId,
  pub sheet_id: SheetId,
  pub originator: AnnotationOriginator,
  pub points: Vec<BoardPoint>,
  pub style: StrokeStyle,
  pub bbox: BoardRect,
}
