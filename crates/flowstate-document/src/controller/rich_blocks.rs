use std::collections::BTreeMap;
use std::io;

use flowstate_collab::{
  FlowId, FlowMaterialization, FlowNode, FlowNodeId, FlowNodeKind, FlowNodeRecord, FlowSeedFlow, FlowSeedNode, MaterializedFlow,
  MaterializedObjectGraph,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{flow_marks_from_runs, serialize_block_metadata, serialize_paragraph_metadata};
use crate::{
  Block, BlockId, EquationBlock, ImageBlock, Paragraph, ParagraphId, ParagraphStyle, RichBlockIdentity, RunStyles, TableBlock, TableCell,
  TableCellBlock, TableCellBlockIdentity, TableCellIdentity, TableCellParagraph, TableIdentity, TableRow, TableRowIdentity, TextRun,
  deserialize_block_metadata,
};

const RICH_METADATA_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RichNodeMetadata {
  version: u32,
  kind: RichNodeKind,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
enum RichNodeKind {
  BlockShell(Vec<u8>),
  TableRow,
  TableCell { row_span: u16, col_span: u16 },
}

pub(super) fn seed_object(block_id: FlowNodeId, block: &Block, flows: &mut Vec<FlowSeedFlow>) -> io::Result<FlowSeedNode> {
  let (shell, child_flows) = match block {
    Block::Paragraph(_) => return Err(io::Error::new(io::ErrorKind::InvalidInput, "paragraph cannot be seeded as a rich object")),
    Block::Image(image) => seed_image(block_id, image, flows)?,
    Block::Equation(equation) => seed_equation(block_id, equation, flows)?,
    Block::Table(table) => seed_table(block_id, table, flows)?,
  };
  object_seed(
    block_id,
    RichNodeKind::BlockShell(serialize_block_metadata(&shell)),
    child_flows,
  )
}

pub(super) fn seed_table_row(
  row_id: FlowNodeId,
  cells: &[(FlowNodeId, FlowNodeId)],
  flows: &mut Vec<FlowSeedFlow>,
) -> io::Result<FlowSeedNode> {
  if cells.is_empty() {
    return Err(io::Error::new(io::ErrorKind::InvalidInput, "table row must contain at least one cell"));
  }
  let cells_flow = FlowId::new();
  let cell_nodes = cells
    .iter()
    .map(|(cell_id, paragraph_id)| seed_table_cell(*cell_id, *paragraph_id, flows))
    .collect::<io::Result<Vec<_>>>()?;
  flows.push(FlowSeedFlow {
    id: cells_flow,
    nodes: cell_nodes,
  });
  object_seed(row_id, RichNodeKind::TableRow, vec![cells_flow])
}

pub(super) fn seed_table_cell(cell_id: FlowNodeId, paragraph_id: FlowNodeId, flows: &mut Vec<FlowSeedFlow>) -> io::Result<FlowSeedNode> {
  let content_flow = FlowId::new();
  flows.push(FlowSeedFlow {
    id: content_flow,
    nodes: vec![paragraph_seed(paragraph_id, ParagraphStyle::Normal, &[], "")?],
  });
  object_seed(
    cell_id,
    RichNodeKind::TableCell {
      row_span: 1,
      col_span: 1,
    },
    vec![content_flow],
  )
}

pub(super) fn table_shell_metadata(table: &TableBlock) -> io::Result<Vec<u8>> {
  let mut shell = table.clone();
  shell.rows.clear();
  postcard::to_stdvec(&RichNodeMetadata {
    version: RICH_METADATA_VERSION,
    kind: RichNodeKind::BlockShell(serialize_block_metadata(&Block::Table(shell))),
  })
  .map_err(invalid_data)
}

pub(super) fn image_shell_metadata(image: &ImageBlock) -> io::Result<Vec<u8>> {
  let mut shell = image.clone();
  shell.caption = None;
  postcard::to_stdvec(&RichNodeMetadata {
    version: RICH_METADATA_VERSION,
    kind: RichNodeKind::BlockShell(serialize_block_metadata(&Block::Image(shell))),
  })
  .map_err(invalid_data)
}

pub(super) fn materialize_object(record: &FlowNodeRecord, materialized: &FlowMaterialization) -> io::Result<Block> {
  materialize_object_parts(record, &materialized.assets, &materialized.flows)
}

pub(super) fn materialize_object_graph(materialized: &MaterializedObjectGraph) -> io::Result<Block> {
  materialize_object_parts(&materialized.root, &materialized.assets, &materialized.flows)
}

fn materialize_object_parts(
  record: &FlowNodeRecord,
  assets: &BTreeMap<flowstate_collab::FlowAssetId, flowstate_collab::FlowAssetReference>,
  flows: &BTreeMap<FlowId, MaterializedFlow>,
) -> io::Result<Block> {
  let metadata = match postcard::from_bytes::<RichNodeMetadata>(&record.metadata) {
    Ok(metadata) if metadata.version == RICH_METADATA_VERSION => metadata,
    _ => return deserialize_block_metadata(&record.metadata),
  };
  let RichNodeKind::BlockShell(shell) = metadata.kind else {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "root DB8 object is not a block shell"));
  };
  let mut block = deserialize_block_metadata(&shell)?;
  match &mut block {
    Block::Paragraph(_) => return Err(io::Error::new(io::ErrorKind::InvalidData, "rich object shell decoded as paragraph")),
    Block::Image(image) => {
      if !assets
        .contains_key(&flowstate_collab::FlowAssetId(Uuid::from_u128(image.asset_id.0)))
      {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "image object references missing source asset"));
      }
      materialize_image(image, record, flows)?;
    },
    Block::Equation(equation) => materialize_equation(equation, record, flows)?,
    Block::Table(table) => materialize_table(table, record, flows)?,
  }
  Ok(block)
}

pub(super) fn materialize_object_identity(
  record: &FlowNodeRecord,
  materialized: &FlowMaterialization,
) -> io::Result<RichBlockIdentity> {
  materialize_object_identity_record(record, &materialized.flows)
}

pub(super) fn materialize_object_graph_identity(materialized: &MaterializedObjectGraph) -> io::Result<RichBlockIdentity> {
  materialize_object_identity_record(&materialized.root, &materialized.flows)
}

fn seed_image(block_id: FlowNodeId, image: &ImageBlock, flows: &mut Vec<FlowSeedFlow>) -> io::Result<(Block, Vec<FlowId>)> {
  let mut shell = image.clone();
  shell.caption = None;
  let mut children = Vec::new();
  if let Some(caption) = &image.caption {
    let flow_id = derived_flow_id(block_id.0, "image-caption-flow");
    let paragraph_id = derived_node_id(block_id.0, "image-caption-paragraph");
    flows.push(FlowSeedFlow {
      id: flow_id,
      nodes: vec![paragraph_seed(paragraph_id, caption.style, &caption.runs, "")?],
    });
    children.push(flow_id);
  }
  Ok((Block::Image(shell), children))
}

fn seed_equation(block_id: FlowNodeId, equation: &EquationBlock, flows: &mut Vec<FlowSeedFlow>) -> io::Result<(Block, Vec<FlowId>)> {
  let mut shell = equation.clone();
  shell.source = "".into();
  let flow_id = derived_flow_id(block_id.0, "equation-source-flow");
  let paragraph_id = equation_source_paragraph_id(block_id);
  flows.push(FlowSeedFlow {
    id: flow_id,
    nodes: vec![paragraph_seed(
      paragraph_id,
      ParagraphStyle::Normal,
      &[TextRun {
        len: equation.source.len(),
        styles: RunStyles::default(),
      }],
      equation.source.as_ref(),
    )?],
  });
  Ok((Block::Equation(shell), vec![flow_id]))
}

pub(super) fn equation_source_paragraph_id(block_id: FlowNodeId) -> FlowNodeId {
  derived_node_id(block_id.0, "equation-source-paragraph")
}

fn seed_table(block_id: FlowNodeId, table: &TableBlock, flows: &mut Vec<FlowSeedFlow>) -> io::Result<(Block, Vec<FlowId>)> {
  let mut shell = table.clone();
  shell.rows.clear();
  if table.rows.is_empty() {
    return Ok((Block::Table(shell), Vec::new()));
  }

  let rows_flow = derived_flow_id(block_id.0, "table-rows-flow");
  let mut row_nodes = Vec::with_capacity(table.rows.len());
  for (row_ix, row) in table.rows.iter().enumerate() {
    let row_id = derived_node_id(block_id.0, &format!("table-row-{row_ix}"));
    let mut row_children = Vec::new();
    if !row.cells.is_empty() {
      let cells_flow = derived_flow_id(row_id.0, "table-cells-flow");
      let mut cell_nodes = Vec::with_capacity(row.cells.len());
      for (cell_ix, cell) in row.cells.iter().enumerate() {
        let cell_id = derived_node_id(row_id.0, &format!("table-cell-{cell_ix}"));
        let content_flow = derived_flow_id(cell_id.0, "table-cell-content-flow");
        let content_nodes = seed_cell_content(cell_id, &cell.blocks, flows)?;
        flows.push(FlowSeedFlow {
          id: content_flow,
          nodes: content_nodes,
        });
        cell_nodes.push(object_seed(
          cell_id,
          RichNodeKind::TableCell {
            row_span: cell.row_span,
            col_span: cell.col_span,
          },
          vec![content_flow],
        )?);
      }
      flows.push(FlowSeedFlow {
        id: cells_flow,
        nodes: cell_nodes,
      });
      row_children.push(cells_flow);
    }
    row_nodes.push(object_seed(row_id, RichNodeKind::TableRow, row_children)?);
  }
  flows.push(FlowSeedFlow {
    id: rows_flow,
    nodes: row_nodes,
  });
  Ok((Block::Table(shell), vec![rows_flow]))
}

fn seed_cell_content(namespace: FlowNodeId, blocks: &[TableCellBlock], flows: &mut Vec<FlowSeedFlow>) -> io::Result<Vec<FlowSeedNode>> {
  if blocks.is_empty() {
    return Ok(vec![paragraph_seed(
      derived_node_id(namespace.0, "empty-cell-paragraph"),
      ParagraphStyle::Normal,
      &[],
      "",
    )?]);
  }
  blocks
    .iter()
    .enumerate()
    .map(|(block_ix, block)| match block {
      TableCellBlock::Paragraph(paragraph) => paragraph_seed(
        derived_node_id(namespace.0, &format!("cell-paragraph-{block_ix}")),
        paragraph.paragraph.style,
        &paragraph.paragraph.runs,
        &paragraph.text,
      ),
      TableCellBlock::Table(table) => seed_object(
        derived_node_id(namespace.0, &format!("cell-table-{block_ix}")),
        &Block::Table(table.clone()),
        flows,
      ),
    })
    .collect()
}

fn paragraph_seed(id: FlowNodeId, style: ParagraphStyle, runs: &[TextRun], text: &str) -> io::Result<FlowSeedNode> {
  Ok(FlowSeedNode {
    record: FlowNodeRecord {
      id,
      kind: FlowNodeKind::Paragraph,
      metadata: serialize_paragraph_metadata(style, runs)?,
      child_flows: Vec::new(),
    },
    text: text.to_string(),
    marks: flow_marks_from_runs(runs),
  })
}

fn object_seed(id: FlowNodeId, kind: RichNodeKind, child_flows: Vec<FlowId>) -> io::Result<FlowSeedNode> {
  Ok(FlowSeedNode {
    record: FlowNodeRecord {
      id,
      kind: FlowNodeKind::Object,
      metadata: postcard::to_stdvec(&RichNodeMetadata {
        version: RICH_METADATA_VERSION,
        kind,
      })
      .map_err(invalid_data)?,
      child_flows,
    },
    text: String::new(),
    marks: Vec::new(),
  })
}

fn materialize_image(image: &mut ImageBlock, record: &FlowNodeRecord, flows: &BTreeMap<FlowId, MaterializedFlow>) -> io::Result<()> {
  image.caption = match record.child_flows.as_slice() {
    [] => None,
    [flow_id] => Some(first_paragraph(flow(flows, *flow_id)?)?.0),
    _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "image object owns multiple caption flows")),
  };
  Ok(())
}

fn materialize_equation(
  equation: &mut EquationBlock,
  record: &FlowNodeRecord,
  flows: &BTreeMap<FlowId, MaterializedFlow>,
) -> io::Result<()> {
  let [flow_id] = record.child_flows.as_slice() else {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "equation object must own one source flow"));
  };
  equation.source = flow_paragraph_text(flow(flows, *flow_id)?).into();
  Ok(())
}

fn materialize_table(table: &mut TableBlock, record: &FlowNodeRecord, flows: &BTreeMap<FlowId, MaterializedFlow>) -> io::Result<()> {
  table.rows = match record.child_flows.as_slice() {
    [] => Vec::new(),
    [rows_flow] => flow(flows, *rows_flow)?
      .nodes
      .iter()
      .map(|node| materialize_row(node, flows))
      .collect::<io::Result<Vec<_>>>()?,
    _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "table object owns multiple row flows")),
  };
  Ok(())
}

fn materialize_row(node: &FlowNode, flows: &BTreeMap<FlowId, MaterializedFlow>) -> io::Result<TableRow> {
  let FlowNode::Object { record } = node else {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "table row flow contains paragraph"));
  };
  require_kind(record, |kind| matches!(kind, RichNodeKind::TableRow), "table row metadata invalid")?;
  let cells = match record.child_flows.as_slice() {
    [] => Vec::new(),
    [cells_flow] => flow(flows, *cells_flow)?
      .nodes
      .iter()
      .map(|node| materialize_cell(node, flows))
      .collect::<io::Result<Vec<_>>>()?,
    _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "table row owns multiple cell flows")),
  };
  Ok(TableRow { cells })
}

fn materialize_cell(node: &FlowNode, flows: &BTreeMap<FlowId, MaterializedFlow>) -> io::Result<TableCell> {
  let FlowNode::Object { record } = node else {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "table cell flow contains paragraph"));
  };
  let metadata = decode_metadata(record)?;
  let RichNodeKind::TableCell { row_span, col_span } = metadata.kind else {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "table cell metadata invalid"));
  };
  let [content_flow] = record.child_flows.as_slice() else {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "table cell must own one content flow"));
  };
  Ok(TableCell {
    blocks: materialize_cell_content(flow(flows, *content_flow)?, flows)?,
    row_span,
    col_span,
  })
}

fn materialize_cell_content(flow: &MaterializedFlow, flows: &BTreeMap<FlowId, MaterializedFlow>) -> io::Result<Vec<TableCellBlock>> {
  flow
    .nodes
    .iter()
    .map(|node| match node {
      FlowNode::Paragraph { .. } => {
        let (paragraph, text) = paragraph(node)?;
        Ok(TableCellBlock::Paragraph(TableCellParagraph { paragraph, text }))
      },
      FlowNode::Object { record } => match materialize_object_record(record, flows)? {
        Block::Table(table) => Ok(TableCellBlock::Table(table)),
        Block::Paragraph(_) | Block::Image(_) | Block::Equation(_) => {
          Err(io::Error::new(io::ErrorKind::InvalidData, "unsupported object in table cell flow"))
        },
      },
    })
    .collect()
}

fn materialize_object_record(record: &FlowNodeRecord, flows: &BTreeMap<FlowId, MaterializedFlow>) -> io::Result<Block> {
  let metadata = decode_metadata(record)?;
  let RichNodeKind::BlockShell(shell) = metadata.kind else {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "content object is not a block shell"));
  };
  let mut block = deserialize_block_metadata(&shell)?;
  match &mut block {
    Block::Table(table) => materialize_table(table, record, flows)?,
    Block::Equation(equation) => materialize_equation(equation, record, flows)?,
    Block::Image(image) => materialize_image(image, record, flows)?,
    Block::Paragraph(_) => return Err(io::Error::new(io::ErrorKind::InvalidData, "content object decoded as paragraph")),
  }
  Ok(block)
}

fn materialize_object_identity_record(record: &FlowNodeRecord, flows: &BTreeMap<FlowId, MaterializedFlow>) -> io::Result<RichBlockIdentity> {
  let metadata = decode_metadata(record)?;
  let RichNodeKind::BlockShell(shell) = metadata.kind else {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "content object is not a block shell"));
  };
  match deserialize_block_metadata(&shell)? {
    Block::Image(_) => {
      let caption = match record.child_flows.as_slice() {
        [] => None,
        [caption_flow] => Some(first_paragraph_id(flow(flows, *caption_flow)?)?),
        _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "image object owns multiple caption flows")),
      };
      Ok(RichBlockIdentity::Image { caption })
    },
    Block::Equation(_) => {
      let [source_flow] = record.child_flows.as_slice() else {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "equation object must own one source flow"));
      };
      Ok(RichBlockIdentity::Equation {
        source: first_paragraph_id(flow(flows, *source_flow)?)?,
      })
    },
    Block::Table(_) => materialize_table_identity(record, flows).map(RichBlockIdentity::Table),
    Block::Paragraph(_) => Err(io::Error::new(io::ErrorKind::InvalidData, "content object decoded as paragraph")),
  }
}

fn materialize_table_identity(record: &FlowNodeRecord, flows: &BTreeMap<FlowId, MaterializedFlow>) -> io::Result<TableIdentity> {
  let rows = match record.child_flows.as_slice() {
    [] => Vec::new(),
    [rows_flow] => flow(flows, *rows_flow)?
      .nodes
      .iter()
      .map(|node| materialize_row_identity(node, flows))
      .collect::<io::Result<Vec<_>>>()?,
    _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "table object owns multiple row flows")),
  };
  Ok(TableIdentity { rows })
}

fn materialize_row_identity(node: &FlowNode, flows: &BTreeMap<FlowId, MaterializedFlow>) -> io::Result<TableRowIdentity> {
  let FlowNode::Object { record } = node else {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "table row flow contains paragraph"));
  };
  require_kind(record, |kind| matches!(kind, RichNodeKind::TableRow), "table row metadata invalid")?;
  let cells = match record.child_flows.as_slice() {
    [] => Vec::new(),
    [cells_flow] => flow(flows, *cells_flow)?
      .nodes
      .iter()
      .map(|node| materialize_cell_identity(node, flows))
      .collect::<io::Result<Vec<_>>>()?,
    _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "table row owns multiple cell flows")),
  };
  Ok(TableRowIdentity {
    id: BlockId(record.id.0.as_u128()),
    cells,
  })
}

fn materialize_cell_identity(node: &FlowNode, flows: &BTreeMap<FlowId, MaterializedFlow>) -> io::Result<TableCellIdentity> {
  let FlowNode::Object { record } = node else {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "table cell flow contains paragraph"));
  };
  let [content_flow] = record.child_flows.as_slice() else {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "table cell must own one content flow"));
  };
  let blocks = flow(flows, *content_flow)?
    .nodes
    .iter()
    .map(|node| match node {
      FlowNode::Paragraph { record, .. } => Ok(TableCellBlockIdentity::Paragraph(ParagraphId(record.id.0.as_u128()))),
      FlowNode::Object { record } => match materialize_object_identity_record(record, flows)? {
        RichBlockIdentity::Table(identity) => Ok(TableCellBlockIdentity::Table {
          id: BlockId(record.id.0.as_u128()),
          identity,
        }),
        RichBlockIdentity::Image { .. } | RichBlockIdentity::Equation { .. } => {
          Err(io::Error::new(io::ErrorKind::InvalidData, "unsupported object in table cell identity"))
        },
      },
    })
    .collect::<io::Result<Vec<_>>>()?;
  Ok(TableCellIdentity {
    id: BlockId(record.id.0.as_u128()),
    blocks,
  })
}

fn paragraph(node: &FlowNode) -> io::Result<(Paragraph, String)> {
  let FlowNode::Paragraph { record, text, marks } = node else {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "expected paragraph node"));
  };
  let (style, _) = super::deserialize_paragraph_metadata(&record.metadata)
    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "rich-flow paragraph metadata invalid"))?;
  let runs = crate::db8_runs_from_marks(text.len(), &super::granular_marks(marks));
  Ok((
    Paragraph {
      style,
      byte_range: 0..text.len(),
      runs,
      version: 0,
    },
    text.clone(),
  ))
}

fn first_paragraph(flow: &MaterializedFlow) -> io::Result<(Paragraph, String)> {
  paragraph(
    flow
      .nodes
      .first()
      .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "rich child flow is empty"))?,
  )
}

fn first_paragraph_id(flow: &MaterializedFlow) -> io::Result<ParagraphId> {
  let node = flow
    .nodes
    .first()
    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "rich child flow is empty"))?;
  let FlowNode::Paragraph { record, .. } = node else {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "rich child flow does not begin with paragraph"));
  };
  Ok(ParagraphId(record.id.0.as_u128()))
}

fn flow_paragraph_text(flow: &MaterializedFlow) -> String {
  flow
    .nodes
    .iter()
    .filter_map(|node| match node {
      FlowNode::Paragraph { text, .. } => Some(text.as_str()),
      FlowNode::Object { .. } => None,
    })
    .collect::<Vec<_>>()
    .join("\n")
}

fn flow(flows: &BTreeMap<FlowId, MaterializedFlow>, flow_id: FlowId) -> io::Result<&MaterializedFlow> {
  flows
    .get(&flow_id)
    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "rich child flow missing"))
}

fn decode_metadata(record: &FlowNodeRecord) -> io::Result<RichNodeMetadata> {
  let metadata: RichNodeMetadata = postcard::from_bytes(&record.metadata).map_err(invalid_data)?;
  if metadata.version != RICH_METADATA_VERSION {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "unsupported rich node metadata version"));
  }
  Ok(metadata)
}

fn require_kind(
  record: &FlowNodeRecord,
  expected: impl FnOnce(&RichNodeKind) -> bool,
  message: &'static str,
) -> io::Result<()> {
  let metadata = decode_metadata(record)?;
  if expected(&metadata.kind) {
    Ok(())
  } else {
    Err(io::Error::new(io::ErrorKind::InvalidData, message))
  }
}

fn derived_node_id(namespace: Uuid, label: &str) -> FlowNodeId {
  FlowNodeId(Uuid::new_v5(&namespace, label.as_bytes()))
}

fn derived_flow_id(namespace: Uuid, label: &str) -> FlowId {
  FlowId(Uuid::new_v5(&namespace, label.as_bytes()))
}

fn invalid_data(error: impl std::error::Error + Send + Sync + 'static) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, error)
}
