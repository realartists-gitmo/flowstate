use std::path::PathBuf;
use std::sync::OnceLock;

use gpui::{AnyWindowHandle, Entity, TestAppContext, Window};

use crate::workspace::{Workspace, open_workspace_window};

/// Process-wide sandbox for every on-disk artifact the app touches: settings
/// (incl. the first-run profile mint), the tub data dir, and the open-tabs
/// session file. Set once, before the first `load_app_settings`, while all
/// other test threads are still blocked on the `OnceLock`.
fn sandbox_root() -> &'static PathBuf {
  static SANDBOX: OnceLock<PathBuf> = OnceLock::new();
  SANDBOX.get_or_init(|| {
    let root = std::env::temp_dir().join(format!("flowstate-headless-{}", std::process::id()));
    let config = root.join("config");
    let data = root.join("data");
    std::fs::create_dir_all(config.join("flowstate")).expect("create sandbox config dir");
    std::fs::create_dir_all(&data).expect("create sandbox data dir");
    // Discovery stays paused so no test ever reaches a real transport
    // (BLE/Dropbox). BLE is opt-in-default-off anyway; the pause covers all.
    std::fs::write(config.join("flowstate/settings.toml"), "collaboration_discovery_paused = true\n").expect("write sandbox settings");
    // SAFETY: single writer inside OnceLock init; every test enters through
    // this function before any env read of these keys, and concurrent
    // first-callers are parked on the OnceLock until it returns.
    unsafe { std::env::set_var("FLOWSTATE_CONFIG_DIR", &config) };
    // SAFETY: same single-writer OnceLock-init guarantee as above.
    unsafe { std::env::set_var("FLOWSTATE_DATA_DIR", &data) };
    root
  })
}

pub fn sandbox_config_dir() -> PathBuf {
  sandbox_root().join("config")
}

pub struct WorkspaceHarness {
  pub window: AnyWindowHandle,
  pub workspace: Entity<Workspace>,
}

/// A captured accessibility tree, with the queries assertions actually want.
///
/// gpui dumps a FLAT map of `ephemeral id -> node`, where each node carries its
/// `children` as ephemeral ids, so every structural question here is a lookup
/// through that map rather than a walk of nested json.
pub struct A11yTree(pub serde_json::Value);

impl A11yTree {
  fn nodes(&self) -> impl Iterator<Item = (&String, &serde_json::Value)> {
    self
      .0
      .get("nodes")
      .and_then(|n| n.as_object())
      .into_iter()
      .flat_map(|map| map.iter())
  }

  pub fn len(&self) -> usize {
    self.nodes().count()
  }

  /// `aria.role` for a node, e.g. `"Button"`.
  fn role_of(node: &serde_json::Value) -> Option<&str> {
    node.get("aria")?.get("role")?.as_str()
  }

  /// The accessible NAME: `aria.label`, falling back to `aria.value` the way a
  /// screen reader would when a control has no label of its own.
  fn name_of(node: &serde_json::Value) -> Option<&str> {
    let aria = node.get("aria")?;
    aria
      .get("label")
      .and_then(|v| v.as_str())
      .or_else(|| aria.get("value").and_then(|v| v.as_str()))
  }

  pub fn by_role(&self, role: &str) -> Vec<&serde_json::Value> {
    self
      .nodes()
      .filter(|(_, n)| Self::role_of(n) == Some(role))
      .map(|(_, n)| n)
      .collect()
  }

  pub fn roles(&self) -> Vec<String> {
    let mut roles: Vec<String> = self.nodes().filter_map(|(_, n)| Self::role_of(n).map(str::to_string)).collect();
    roles.sort();
    roles.dedup();
    roles
  }

  /// First node whose accessible name equals `name`.
  pub fn by_name(&self, name: &str) -> Option<&serde_json::Value> {
    self.nodes().map(|(_, n)| n).find(|n| Self::name_of(n) == Some(name))
  }

  pub fn names(&self) -> Vec<String> {
    self.nodes().filter_map(|(_, n)| Self::name_of(n).map(str::to_string)).collect()
  }

  /// Nodes that advertise an action but carry no accessible name — the exact
  /// shape a screen reader announces as an anonymous "button".
  pub fn actionable_without_name(&self) -> Vec<String> {
    self
      .nodes()
      .filter(|(_, n)| {
        let acts = n.get("aria").and_then(|a| a.get("on_action")).and_then(|a| a.as_array());
        let interactive = acts.is_some_and(|a| a.iter().any(|x| x.as_str() == Some("Click")));
        interactive && Self::name_of(n).is_none()
      })
      .map(|(id, n)| format!("{id} ({})", Self::role_of(n).unwrap_or("?")))
      .collect()
  }

  /// The node gpui reports as focused, if any.
  ///
  /// The dump records it as an ephemeral node id under `gpui_focus`; `None`
  /// means gpui found no reportable node and fell back to the window root —
  /// which is exactly the failure that `.id()` + `.role()` + `.track_focus()`
  /// on one element prevents.
  pub fn focused_node(&self) -> Option<&serde_json::Value> {
    let focus_id = self.0.get("gpui_focus").and_then(|f| f.as_str())?;
    self.0.get("nodes")?.get(focus_id)
  }

  /// The STABLE AccessKit id of the paragraph whose text runs contain `text`.
  ///
  /// `accesskit_id` is the real node id (a hash of the element's
  /// `GlobalElementId`), unlike the short `a`/`b`/`c` keys, which are just
  /// per-dump ordinals and would compare unequal for unrelated reasons.
  pub fn accesskit_id_of_text(&self, text: &str) -> Option<String> {
    let nodes = self.0.get("nodes")?.as_object()?;
    // Find the run carrying the text, then the paragraph that lists it as a child.
    let run_key = nodes.iter().find_map(|(key, node)| {
      let aria = node.get("aria")?;
      (aria.get("role")?.as_str()? == "TextRun" && aria.get("value")?.as_str()?.contains(text)).then(|| key.clone())
    })?;
    nodes.iter().find_map(|(_, node)| {
      let children = node.get("children")?.as_array()?;
      children
        .iter()
        .any(|c| c.as_str() == Some(run_key.as_str()))
        .then(|| node.get("accesskit_id")?.as_str().map(str::to_string))
        .flatten()
    })
  }

  /// Pretty-printed dump, for `--nocapture` debugging of a failing assertion.
  pub fn dump(&self) -> String {
    serde_json::to_string_pretty(&self.0).unwrap_or_default()
  }
}

impl WorkspaceHarness {
  /// Run `f` against the workspace with the window available — the same shape
  /// as a real event handler (workspace lease held, window borrowed).
  pub fn update<R>(&self, cx: &mut TestAppContext, f: impl FnOnce(&mut Workspace, &mut Window, &mut gpui::Context<Workspace>) -> R) -> R {
    let workspace = self.workspace.clone();
    self
      .window
      .update(cx, |_, window, cx| workspace.update(cx, |ws, cx| f(ws, window, cx)))
      .expect("workspace window is open")
  }

  /// The window's accessibility tree, as the flat node map gpui dumps.
  ///
  /// Only works because `vendor/gpui` patches `TestWindow::a11y_init` to
  /// activate accessibility — upstream's default is a no-op, which leaves
  /// `A11y::is_active()` false, so `draw_roots` never calls `begin_frame` and
  /// this returns `None`. If that patch is ever lost this panics rather than
  /// silently asserting against an empty tree.
  pub fn a11y(&self, cx: &mut TestAppContext) -> A11yTree {
    let json = self
      .window
      .update(cx, |_, window, _| window.debug_a11y_tree_json())
      .expect("workspace window is open")
      .expect("no a11y tree captured — is the vendor/gpui TestWindow::a11y_init patch still present?");
    A11yTree(serde_json::from_str(&json).expect("a11y dump is valid json"))
  }

  pub fn read<R>(&self, cx: &mut TestAppContext, f: impl FnOnce(&Workspace) -> R) -> R {
    let workspace = self.workspace.clone();
    self
      .window
      .update(cx, |_, _, cx| f(workspace.read(cx)))
      .expect("workspace window is open")
  }

  /// Create a blank in-memory document panel and wait for the quiet state.
  pub fn new_document(&self, cx: &mut TestAppContext) {
    self.update(cx, |ws, window, cx| ws.new_document(window, cx));
    cx.run_until_parked();
  }

  /// Wait (real time — document runtimes live on OS threads outside the test
  /// dispatcher) until `ready` observes the state it wants, or panic.
  pub fn wait_until(&self, cx: &mut TestAppContext, what: &str, mut ready: impl FnMut(&Workspace) -> bool) {
    for _ in 0..500 {
      cx.run_until_parked();
      if self.read(cx, |ws| ready(ws)) {
        return;
      }
      std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("timed out waiting for: {what}");
  }
}

/// Boot the real app wiring headlessly: component/theme globals, the real
/// keymap, and the production `open_workspace_window` path (close-prompt
/// install, session restore, initial frame). Deliberately does NOT install
/// the custom prompt renderer — prompts must stay on the test platform's
/// queue so tests can drive them with `simulate_prompt_answer`.
pub fn open_workspace(cx: &mut TestAppContext) -> WorkspaceHarness {
  sandbox_root();
  // The workspace starts the real `flowstate-doc-io` service thread (see
  // `DocIoHandle::spawn`), which wakes gpui tasks from a non-test thread. Since
  // the gpui upgrade, `TestScheduler` asserts that all activity happens on the
  // test thread and otherwise fails the test with "Your test is not
  // deterministic". `allow_parking` is gpui's sanctioned opt-out for exactly
  // this shape — "a mix of deterministic and non-deterministic async behavior,
  // such as when interacting with I/O in an otherwise deterministic test" — and
  // also lets `run_until_parked` block on that thread's replies instead of
  // panicking with "Parking forbidden".
  cx.executor().allow_parking();
  cx.update(|cx| {
    gpui_component::init(cx);
    crate::app::register_rich_text_editor_keybindings(cx);
  });
  let workspace = cx.update(|cx| open_workspace_window(None, cx));
  cx.run_until_parked();
  let workspace = workspace
    .upgrade()
    .expect("workspace entity alive after window open");
  let window = *cx.windows().first().expect("workspace window exists");
  WorkspaceHarness { window, workspace }
}
