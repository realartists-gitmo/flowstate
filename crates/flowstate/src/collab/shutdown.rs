use flowstate_collab::net::NetCommand;
use gpui::BorrowAppContext;

use super::CollabManager;

impl CollabManager {
  pub(super) fn shutdown_runtime(&mut self) {
    if let Some(runtime) = self.runtime.take() {
      let _ = runtime.commands.try_send(NetCommand::Shutdown);
    }
    self.sessions_by_id.clear();
    self.session_by_panel.clear();
    self.event_pump_started = false;
    self.endpoint_online = false;
  }
}

pub fn shutdown<C>(cx: &mut C)
where
  C: BorrowAppContext,
{
  cx.update_default_global::<CollabManager, _>(|manager, _| manager.shutdown_runtime());
}
