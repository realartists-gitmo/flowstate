use wasmtime::StoreLimits;
use wasmtime::component::ResourceTable;
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};
use wasmtime_wasi_http::{WasiHttpCtx, WasiHttpView};

use super::{DirectoryAccess, DirectoryGrantResponse, ExtensionHost, HostError, flowstate};

pub(super) struct State<H> {
    wasi: WasiCtx,
    http: WasiHttpCtx,
    table: ResourceTable,
    pub(super) limits: StoreLimits,
    host: H,
}

impl<H> State<H> {
    pub(super) fn new(wasi: WasiCtx, limits: StoreLimits, host: H) -> Self {
        Self { wasi, http: WasiHttpCtx::new(), table: ResourceTable::new(), limits, host }
    }
}

impl<H: Send> WasiHttpView for State<H> {
    fn ctx(&mut self) -> &mut WasiHttpCtx { &mut self.http }
    fn table(&mut self) -> &mut ResourceTable { &mut self.table }
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
    fn request_directory_access(
        &mut self,
        mode: flowstate::extension::host::DirectoryAccess,
        suggested_path: Option<String>,
    ) -> Result<flowstate::extension::host::DirectoryGrant, flowstate::extension::host::HostError> {
        let mode = match mode {
            flowstate::extension::host::DirectoryAccess::Read => DirectoryAccess::Read,
            flowstate::extension::host::DirectoryAccess::ReadWrite => DirectoryAccess::ReadWrite,
        };
        self.host.request_directory_access(mode, suggested_path.as_deref()).map(Into::into).map_err(Into::into)
    }
}

impl From<DirectoryGrantResponse> for flowstate::extension::host::DirectoryGrant {
    fn from(grant: DirectoryGrantResponse) -> Self {
        Self { grant_id: grant.grant_id, mount_path: grant.mount_path, available_next_invocation: grant.available_next_invocation }
    }
}

impl From<HostError> for flowstate::extension::host::HostError {
    fn from(error: HostError) -> Self { Self { code: error.code, message: error.message } }
}
