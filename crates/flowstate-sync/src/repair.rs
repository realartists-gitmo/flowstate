#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RepairAction {
  RetainedReplay,
  SnapshotRepair,
}

pub(crate) fn choose_repair_action(retained_durable_updates_available: bool, receiver_lag: usize, retained_range: usize) -> RepairAction {
  if retained_durable_updates_available && receiver_lag <= retained_range {
    RepairAction::RetainedReplay
  } else {
    RepairAction::SnapshotRepair
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn repair_action_uses_retained_replay_when_updates_are_available() {
    assert_eq!(choose_repair_action(true, 0, 0), RepairAction::RetainedReplay);
    assert_eq!(choose_repair_action(true, 2, 3), RepairAction::RetainedReplay);
  }

  #[test]
  fn repair_action_uses_snapshot_when_retention_is_missing() {
    assert_eq!(choose_repair_action(false, 0, 0), RepairAction::SnapshotRepair);
  }

  #[test]
  fn repair_action_uses_snapshot_when_lag_exceeds_retained_range() {
    assert_eq!(choose_repair_action(true, 4, 3), RepairAction::SnapshotRepair);
  }
}
