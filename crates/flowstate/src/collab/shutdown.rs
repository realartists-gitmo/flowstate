use flowstate_collab::net::NetCommand;
use gpui::BorrowAppContext;

use super::CollabManager;

impl CollabManager {
  pub(super) fn shutdown_runtime(&mut self) {
    tracing::info!(open_sessions = self.sessions_by_id.len(), runtime_running = self.runtime.is_some(), "shutting down collaboration runtime");
    if let Some(runtime) = self.runtime.take()
      && let Err(error) = runtime.commands.try_send(NetCommand::Shutdown)
    {
      tracing::warn!(error = %error, "queueing collaboration runtime shutdown failed");
    }
    self.sessions_by_id.clear();
    self.session_by_panel.clear();
    self.event_pump_started = false;
    self.endpoint_online = false;
    tracing::debug!("collaboration runtime shutdown state cleared");
  }
}

pub fn shutdown<C>(cx: &mut C)
where
  C: BorrowAppContext,
{
  cx.update_default_global::<CollabManager, _>(|manager, _| manager.shutdown_runtime());
}
