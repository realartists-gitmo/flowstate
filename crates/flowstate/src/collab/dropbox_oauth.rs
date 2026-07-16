use std::sync::{Mutex, OnceLock};

use anyhow::{Context as _, Result, bail};
use flowstate_collab::dropbox::DropboxPkceFlow;
use gpui::{App, PromptButton, PromptLevel};
use url::Url;

use crate::app_settings::{load_app_settings, save_dropbox_collaboration};

const REDIRECT_URI: &str = "flowstate://oauth/dropbox";

fn pending_flow() -> &'static Mutex<Option<DropboxPkceFlow>> {
  static PENDING: OnceLock<Mutex<Option<DropboxPkceFlow>>> = OnceLock::new();
  PENDING.get_or_init(|| Mutex::new(None))
}

/// R7: the connection ceremony's visible state — the Hub's Discovery room
/// renders it so connect/failure/revoke never live only in a transient
/// prompt.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum DropboxOauthStatus {
  #[default]
  Idle,
  /// `begin` opened the browser; the callback hasn't landed yet.
  Pending,
  Failed(String),
}

fn status_register() -> &'static Mutex<DropboxOauthStatus> {
  static STATUS: OnceLock<Mutex<DropboxOauthStatus>> = OnceLock::new();
  STATUS.get_or_init(|| Mutex::new(DropboxOauthStatus::Idle))
}

pub fn status() -> DropboxOauthStatus {
  status_register()
    .lock()
    .map(|status| status.clone())
    .unwrap_or_default()
}

fn set_status(status: DropboxOauthStatus) {
  if let Ok(mut register) = status_register().lock() {
    *register = status;
  }
}

/// Begin Dropbox desktop OAuth using PKCE. The verifier remains process-local
/// and the callback state is checked before any token is accepted.
pub fn begin(cx: &mut App) -> Result<()> {
  let settings = load_app_settings().dropbox_collaboration;
  let flow = match DropboxPkceFlow::begin(settings.app_key, REDIRECT_URI) {
    Ok(flow) => flow,
    Err(error) => {
      set_status(DropboxOauthStatus::Failed(format!("{error:#}")));
      return Err(error);
    },
  };
  let authorization_url = flow.authorization_url.to_string();
  *pending_flow()
    .lock()
    .expect("Dropbox OAuth state mutex poisoned") = Some(flow);
  set_status(DropboxOauthStatus::Pending);
  cx.open_url(&authorization_url);
  Ok(())
}

/// Route a custom URL if it is the Dropbox OAuth callback. Returns `true`
/// whenever the URL belongs to this route, including malformed callbacks, so
/// it can never fall through to collaboration-ticket decoding.
pub fn route_callback(url: &str, cx: &mut App) -> bool {
  let Ok(callback) = Url::parse(url) else { return false };
  if callback.scheme() != "flowstate" || callback.host_str() != Some("oauth") || callback.path() != "/dropbox" {
    return false;
  }

  let flow = pending_flow()
    .lock()
    .expect("Dropbox OAuth state mutex poisoned")
    .take();
  let root = load_app_settings().dropbox_collaboration.root;
  cx.spawn(async move |cx| {
    let result = async {
      let flow = flow.context("No Dropbox connection was started in this Flowstate process")?;
      let credentials = flow.exchange(&callback).await?;
      save_dropbox_collaboration(credentials, root, true).context("saving Dropbox connection")?;
      Ok::<_, anyhow::Error>(())
    }
    .await;

    match &result {
      Ok(()) => set_status(DropboxOauthStatus::Idle),
      Err(error) => set_status(DropboxOauthStatus::Failed(format!("{error:#}"))),
    }
    let _ = cx.update(|cx| {
      if result.is_ok() {
        crate::collab::reconfigure_discovery(cx);
      }
      show_result(result, cx);
    });
  })
  .detach();
  true
}

fn show_result(result: Result<()>, cx: &mut App) {
  let Some(window_handle) = cx.active_window() else {
    if let Err(error) = result {
      tracing::error!(error = %format_args!("{error:#}"), "Dropbox authorization failed without an active window");
    }
    return;
  };
  let _ = window_handle.update(cx, |_, window, cx| match result {
    Ok(()) => {
      std::mem::drop(window.prompt(
        PromptLevel::Info,
        "Dropbox connected",
        Some("Flowstate can now use Dropbox for explicitly linked documents and collaboration discovery."),
        &[PromptButton::ok("Ok")],
        cx,
      ));
    },
    Err(error) => {
      let detail = format!("Dropbox could not be connected: {error:#}");
      std::mem::drop(window.prompt(
        PromptLevel::Critical,
        "Dropbox connection failed",
        Some(&detail),
        &[PromptButton::ok("Ok")],
        cx,
      ));
    },
  });
}

pub fn cancel_pending() -> Result<()> {
  let removed = pending_flow()
    .lock()
    .map_err(|_| anyhow::anyhow!("Dropbox OAuth state mutex poisoned"))?
    .take();
  if removed.is_none() {
    bail!("No Dropbox connection is pending");
  }
  set_status(DropboxOauthStatus::Idle);
  Ok(())
}
