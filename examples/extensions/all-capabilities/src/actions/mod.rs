mod document;
mod filesystem;
mod network;
mod runtime;

use crate::bindings::flowstate::extension::host::{self, HostError};

pub fn run(action_id: &str) -> Result<(), String> {
    host::set_status(&format!("Running {action_id}…"));
    let result = match action_id {
        "inspect" => document::inspect(),
        "replace-selection" => document::replace_selection(),
        "insert-blocks" => document::insert_blocks(),
        "refresh" => document::refresh(),
        "filesystem" => filesystem::exercise(),
        "network" => network::fetch_example(),
        "request-directory" => filesystem::request_directory(),
        "runtime" => runtime::exercise(),
        _ => Err(format!("unknown action: {action_id}")),
    };
    host::set_status(if result.is_ok() { "Finished" } else { "Failed" });
    result
}

pub(super) fn host_error(error: HostError) -> String {
    format!("{}: {}", error.code, error.message)
}
