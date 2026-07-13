use std::fs;
use std::path::Path;

use super::host_error;
use crate::bindings::flowstate::extension::host::{self, DirectoryAccess};

pub fn exercise() -> Result<(), String> {
    let manifest = fs::read_to_string("/extension/extension.toml")
        .map_err(|error| format!("read /extension: {error}"))?;
    let document_entries = list("/document");
    let grant_entries = list("/grants");
    let line = format!(
        "manifest={} bytes; /document={document_entries:?}; /grants={grant_entries:?}\n",
        manifest.len()
    );
    fs::create_dir_all("/data").map_err(|error| format!("create /data: {error}"))?;
    fs::write("/data/capabilities.log", &line)
        .map_err(|error| format!("write /data: {error}"))?;
    host::set_status(&line);
    Ok(())
}

pub fn request_directory() -> Result<(), String> {
    let grant = host::request_directory_access(DirectoryAccess::ReadWrite, None)
        .map_err(host_error)?;
    host::set_status(&format!(
        "Grant {} mounts at {} (next invocation: {})",
        grant.grant_id, grant.mount_path, grant.available_next_invocation
    ));
    host::set_action_label("request-directory", "Request another directory")
        .map_err(host_error)
}

fn list(path: &str) -> Vec<String> {
    fs::read_dir(Path::new(path))
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect()
}
