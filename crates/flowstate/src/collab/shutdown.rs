use gpui::BorrowAppContext;

use super::CollabManager;

pub fn shutdown<C>(cx: &mut C)
where
  C: BorrowAppContext,
{
  cx.update_default_global::<CollabManager, _>(|manager, _| manager.shutdown_runtime());
}
