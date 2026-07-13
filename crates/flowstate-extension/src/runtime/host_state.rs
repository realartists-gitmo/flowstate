use wasmtime::StoreLimits;
use wasmtime::component::ResourceTable;
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

use super::{ExtensionHost, HostError, flowstate};

pub(super) struct State<H> {
    wasi: WasiCtx,
    table: ResourceTable,
    pub(super) limits: StoreLimits,
    host: H,
}

impl<H> State<H> {
    pub(super) fn new(wasi: WasiCtx, limits: StoreLimits, host: H) -> Self {
        Self { wasi, table: ResourceTable::new(), limits, host }
    }
}

impl<H: Send> WasiView for State<H> {
    fn ctx(&mut self) -> WasiCtxView<'_> { WasiCtxView { ctx: &mut self.wasi, table: &mut self.table } }
}

impl<H: ExtensionHost> flowstate::extension::host::Host for State<H> {
    fn snapshot(&mut self) -> Result<String, flowstate::extension::host::HostError> { self.host.snapshot().map_err(Into::into) }
    fn selection(&mut self) -> Result<String, flowstate::extension::host::HostError> { self.host.selection().map_err(Into::into) }
    fn apply_edits(&mut self, edits_json: String) -> Result<String, flowstate::extension::host::HostError> { self.host.apply_edits(&edits_json).map_err(Into::into) }
    fn refresh_from_disk(&mut self) -> Result<String, flowstate::extension::host::HostError> { self.host.refresh_from_disk().map_err(Into::into) }
    fn set_action_label(&mut self, action_id: String, label: String) -> Result<(), flowstate::extension::host::HostError> { self.host.set_action_label(&action_id, &label).map_err(Into::into) }
    fn set_status(&mut self, message: String) { self.host.set_status(&message); }
}

impl From<HostError> for flowstate::extension::host::HostError {
    fn from(error: HostError) -> Self { Self { code: error.code, message: error.message } }
}
