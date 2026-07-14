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
