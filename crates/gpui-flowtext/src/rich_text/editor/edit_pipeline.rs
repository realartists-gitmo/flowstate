// Loro-first (spec §5/§12): the optimistic edit pipeline is gone. There is no
// capture range, no before/after span diffing, no semantic-command synthesis,
// no local history recording — intents commit through the write authority and
// the projection advances by exact returned patches (local_write_path.rs).
// What survives here is the small post-mutation bookkeeping shared by the
// write path and projection installs.

#[hotpath::measure_all]
impl RichTextEditor {
  pub(super) fn mark_document_changed(&mut self, generation: u64, reconcile_identity: bool, cx: &mut Context<Self>) {
    self.edit_generation = generation;
    if reconcile_identity {
      self.identity_map.reconcile(&self.document);
    }
    self.refresh_save_status();
    self.schedule_recovery_write(cx);
    cx.notify();
  }

  fn notify_after_mutation(&self, cx: &mut Context<Self>) {
    if self.suppress_mutation_notify == 0 {
      cx.notify();
    }
  }

}
