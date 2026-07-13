impl RichTextEditor {
  #[must_use]
  pub fn extension_snapshot(&self) -> ExtensionDocumentSnapshot {
    let range = self.selection.normalized();
    let text_fragment = selected_rich_fragment(&self.document, range.clone());
    let (selection, selected_text, selected_fragment) = match self.selected_block {
      Some(BlockSelection::TableCell {
        block_ix,
        row_ix,
        cell_ix,
      }) => {
        let fragment = self.selected_table_cell_fragment().unwrap_or(text_fragment);
        (
          ExtensionSelection::TableCell {
            block_ix,
            row_ix,
            cell_ix,
            anchor: self.table_cell_anchor,
            head: self.table_cell_caret,
          },
          block_fragment_plain_text(&fragment),
          fragment,
        )
      },
      Some(BlockSelection::Image(block_ix) | BlockSelection::Equation(block_ix) | BlockSelection::Table(block_ix)) => {
        let fragment = self.selected_block_fragment().unwrap_or(text_fragment);
        (
          ExtensionSelection::Object { block_ix },
          block_fragment_plain_text(&fragment),
          fragment,
        )
      },
      None => (
        ExtensionSelection::Text(self.selection.clone()),
        selected_plain_text(&self.document, range),
        text_fragment,
      ),
    };
    ExtensionDocumentSnapshot {
      generation: self.edit_generation,
      document: self.document.clone(),
      selection,
      selected_text,
      selected_fragment,
    }
  }
}
