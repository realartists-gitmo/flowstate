//! Accessibility-tree coverage.
//!
//! These assert against the semantic tree assistive technology actually sees,
//! rather than pixels. That is only possible because `vendor/gpui` patches
//! `TestWindow::a11y_init` to activate accessibility — upstream's default is a
//! no-op, so `Window::debug_a11y_tree_json()` returns `None` under
//! `#[gpui::test]` and there is nothing to assert against.
//!
//! `support::A11yTree` is the query layer; gpui dumps a FLAT `id -> node` map
//! with `children` as id references, so structure questions are lookups.

#[cfg(test)]
mod tests {
  use gpui::TestAppContext;

  use super::super::support;

  /// The gate for every other test in this file: if a11y is not actually
  /// active, `a11y()` panics rather than returning an empty tree that would
  /// make everything below vacuously pass.
  #[gpui::test]
  fn a11y_tree_is_captured_headlessly(cx: &mut TestAppContext) {
    let h = support::open_workspace(cx);
    let tree = h.a11y(cx);

    assert!(
      tree.len() > 1,
      "expected a populated a11y tree, got {} node(s):\n{}",
      tree.len(),
      tree.dump()
    );
    assert!(
      !tree.by_role("Window").is_empty(),
      "expected a Role::Window root; roles present: {:?}",
      tree.roles()
    );
  }

  /// Every control a screen reader can activate must say what it does.
  ///
  /// This is the single highest-value assertion in the file: before the
  /// `Button` tooltip-fallback patch in `vendor/gpui-component`, 30 of the
  /// shell's buttons reached assistive technology as an anonymous "button".
  #[gpui::test]
  fn every_actionable_node_has_a_name(cx: &mut TestAppContext) {
    let h = support::open_workspace(cx);
    h.new_document(cx);
    let tree = h.a11y(cx);

    let anonymous = tree.actionable_without_name();
    assert!(
      anonymous.is_empty(),
      "{} clickable node(s) have no accessible name and would be announced as a bare \
       \"button\": {anonymous:?}",
      anonymous.len()
    );
  }

  /// The document body must reach assistive technology as real text.
  ///
  /// This is the assertion the whole editor workstream exists for: the editor
  /// paints glyphs directly, so without `a11y_synthetic_children` a screen
  /// reader gets an opaque box where the document should be.
  #[gpui::test]
  fn document_text_reaches_the_a11y_tree(cx: &mut TestAppContext) {
    let h = support::open_workspace(cx);
    h.new_document(cx);

    let typed = "Interpretation the aff must defend";
    h.update(cx, |ws, _window, cx| {
      let editor = ws.active_editor.clone().expect("a document is open");
      editor.update(cx, |editor, cx| editor.insert_text_command(typed, cx));
    });
    cx.run_until_parked();

    let tree = h.a11y(cx);

    // The editor itself is a text input, not an opaque widget.
    assert!(
      !tree.by_role("MultilineTextInput").is_empty(),
      "editor did not expose Role::MultilineTextInput; roles: {:?}",
      tree.roles()
    );

    // The typed text is present, as the value of a TextRun.
    let runs = tree.by_role("TextRun");
    let joined: String = runs
      .iter()
      .filter_map(|n| n.get("aria")?.get("value")?.as_str())
      .collect::<Vec<_>>()
      .join("");
    assert!(
      joined.contains(typed),
      "typed text missing from the a11y tree.\nTextRun values: {joined:?}\n{}",
      tree.dump()
    );
  }

  /// The caret must be reported, or a screen reader cannot track where the user
  /// is. Relies on the `vendor/gpui` debug-dump patch that emits
  /// `text_selection` — upstream's dump omits it entirely.
  #[gpui::test]
  fn caret_position_is_reported(cx: &mut TestAppContext) {
    let h = support::open_workspace(cx);
    h.new_document(cx);
    h.update(cx, |ws, _window, cx| {
      let editor = ws.active_editor.clone().expect("a document is open");
      editor.update(cx, |editor, cx| editor.insert_text_command("abcdef", cx));
    });
    cx.run_until_parked();

    let tree = h.a11y(cx);
    let paragraph = tree
      .by_role("Paragraph")
      .into_iter()
      .find(|n| n.get("aria").and_then(|a| a.get("text_selection")).is_some())
      .unwrap_or_else(|| panic!("no paragraph reported a text_selection:\n{}", tree.dump()));

    let sel = &paragraph["aria"]["text_selection"];
    let focus_ix = sel["focus"]["character_index"].as_u64().expect("focus character_index");
    assert_eq!(
      focus_ix, 6,
      "caret should sit after the 6 typed characters, got {focus_ix}\n{}",
      tree.dump()
    );
  }

  /// Headings must carry their level, so AT heading-navigation lands correctly.
  #[gpui::test]
  fn headings_expose_their_level(cx: &mut TestAppContext) {
    let h = support::open_workspace(cx);
    h.new_document(cx);
    h.update(cx, |ws, _window, cx| {
      let editor = ws.active_editor.clone().expect("a document is open");
      editor.update(cx, |editor, cx| {
        editor.insert_text_command("Framework", cx);
        // Style slot 1 is "Pocket" — the top-level section heading.
        editor.set_paragraph_style_for_selection(crate::rich_text_element::ParagraphStyle::Custom(1), cx);
      });
    });
    cx.run_until_parked();

    let tree = h.a11y(cx);
    let headings = tree.by_role("Heading");
    assert!(
      !headings.is_empty(),
      "a Pocket paragraph should expose Role::Heading; roles: {:?}\n{}",
      tree.roles(),
      tree.dump()
    );
    assert!(
      headings
        .iter()
        .any(|n| n.get("aria").and_then(|a| a.get("level")).is_some()),
      "heading exposed no level:\n{}",
      tree.dump()
    );
  }

  /// The flow board is a column-labelled tree, and its speech attribution is
  /// conveyed purely by horizontal position and colour on screen — so without
  /// an explicit column index and label it is unreachable non-visually.
  #[gpui::test]
  fn flow_board_exposes_a_tree(cx: &mut TestAppContext) {
    let h = support::open_workspace(cx);
    h.update(cx, |ws, window, cx| ws.new_flow(window, cx));
    cx.run_until_parked();

    // A fresh flow panel shows the empty state ("choose a debate style"); the
    // BOARD only exists once a flow has been added, so add one.
    h.update(cx, |ws, window, cx| {
      let panel = ws.flow_panels.first().cloned().expect("a flow panel is open");
      let editor = panel.read(cx).editor();
      editor.update(cx, |editor, cx| {
        editor.add_flow(
          flowstate_flow::DebateStyleFlow {
            name: "Test flow".to_string(),
            columns: vec!["1AC".to_string(), "1NC".to_string()],
            columns_switch: None,
            invert: false,
            starter_boxes: None,
          },
          window,
          cx,
        );
      });
    });
    cx.run_until_parked();

    let tree = h.a11y(cx);
    assert!(
      !tree.by_role("Tree").is_empty(),
      "flow board did not expose Role::Tree; roles: {:?}\n{}",
      tree.roles(),
      tree.dump()
    );
    // The box carries its column so speech attribution — conveyed on screen by
    // horizontal position and colour alone — is available non-visually.
    let items = tree.by_role("TreeItem");
    assert!(
      items
        .iter()
        .any(|n| n.get("aria").and_then(|a| a.get("column_index")).is_some()),
      "no flow TreeItem carried a column index:\n{}",
      tree.dump()
    );
  }

  /// Diagnostic: print the whole tree. Not an assertion — this is how you see
  /// what assistive technology actually gets.
  /// `cargo test -p flowstate --lib -- --ignored --nocapture dump_a11y_tree`
  #[gpui::test]
  #[ignore = "diagnostic dump, not an assertion"]
  fn dump_a11y_tree(cx: &mut TestAppContext) {
    let h = support::open_workspace(cx);
    h.new_document(cx);
    let tree = h.a11y(cx);
    println!("=== {} nodes ===", tree.len());
    println!("roles: {:?}", tree.roles());
    println!("names: {:?}", tree.names());
    println!("actionable WITHOUT a name: {:?}", tree.actionable_without_name());
    println!("{}", tree.dump());
  }

  /// The workspace chrome should expose more than just the window: if this
  /// drops to nothing, roles have been lost somewhere in the shell.
  #[gpui::test]
  fn workspace_chrome_exposes_roles(cx: &mut TestAppContext) {
    let h = support::open_workspace(cx);
    h.new_document(cx);
    let tree = h.a11y(cx);

    let roles = tree.roles();
    assert!(
      roles.len() > 1,
      "workspace exposed only {roles:?} — expected chrome roles beyond the window root"
    );
  }
}
