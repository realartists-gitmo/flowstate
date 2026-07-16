use std::{io, path::Path};

use flowstate_collab::{
  doc_io::DocIoHandle,
  dropbox::{DropboxClient, DropboxWriteError},
};
use loro::ExportMode;

use crate::app_settings::{
  load_dropbox_collaboration, load_dropbox_document_binding, save_dropbox_collaboration, save_dropbox_document_binding,
};

/// S12: mirror a saved `.fl0` to its bound Dropbox path — RAW framed bytes,
/// no revision/package machinery (a flow document IS its snapshot). A remote
/// conflict downloads the peer copy, imports its Loro history into the local
/// runtime (flows merge convergently by construction), and re-uploads.
pub async fn sync_bound_flow_file(local_path: &Path, io_handle: &flowstate_collab::flow::FlowIoHandle, bytes: Vec<u8>) -> io::Result<()> {
  let Some(mut binding) = load_dropbox_document_binding(local_path) else {
    return Ok(());
  };
  let Some((credentials, root)) = load_dropbox_collaboration() else {
    return Err(io::Error::new(
      io::ErrorKind::NotConnected,
      "flow is bound to Dropbox, but Dropbox collaboration is disconnected",
    ));
  };
  let client = DropboxClient::new(credentials);
  let metadata = match client
    .put_checkpoint(&binding.remote_path, bytes, binding.revision.as_deref())
    .await
  {
    Ok(metadata) => metadata,
    Err(DropboxWriteError::Conflict { .. }) => {
      let remote = client
        .download(&binding.remote_path)
        .await
        .map_err(|error| io::Error::other(format!("Dropbox conflict download failed: {error:#}")))?;
      let snapshot = flowstate_flow::persistence::decode_snapshot(&remote.bytes)
        .map_err(|error| io::Error::other(format!("Dropbox path does not hold a Flowstate flow: {error:#}")))?;
      io_handle
        .import_remote_update(snapshot)
        .await
        .map_err(|error| io::Error::other(format!("merging Dropbox flow changes failed: {error:#}")))?;
      io_handle
        .save_to(local_path.to_path_buf())
        .await
        .map_err(|error| io::Error::other(format!("saving merged Dropbox flow failed: {error:#}")))?;
      let merged = io_handle
        .encode_bytes()
        .await
        .map_err(|error| io::Error::other(format!("encoding merged Dropbox flow failed: {error:#}")))?;
      client
        .put_checkpoint(&binding.remote_path, merged, Some(&remote.metadata.rev))
        .await
        .map_err(|error| io::Error::other(format!("Dropbox changed again during conflict recovery: {:#}", error.into_anyhow())))?
    },
    Err(DropboxWriteError::Other(error)) => {
      return Err(io::Error::other(format!("saved locally, but Dropbox upload failed: {error:#}")));
    },
  };
  binding.revision = Some(metadata.rev);
  save_dropbox_document_binding(binding)?;
  let refreshed = client.credentials().await;
  save_dropbox_collaboration(refreshed, root, true)
}

/// Mirror a successfully assembled local checkpoint to its explicitly bound
/// Dropbox path. Unbound documents remain strictly local.
pub async fn sync_bound_checkpoint(local_path: &Path, title: String, io_handle: &DocIoHandle, package: Vec<u8>) -> io::Result<()> {
  let Some(mut binding) = load_dropbox_document_binding(local_path) else {
    return Ok(());
  };
  let Some((credentials, root)) = load_dropbox_collaboration() else {
    return Err(io::Error::new(
      io::ErrorKind::NotConnected,
      "document is bound to Dropbox, but Dropbox collaboration is disconnected",
    ));
  };
  let client = DropboxClient::new(credentials);
  let metadata = match client
    .put_checkpoint(&binding.remote_path, package.clone(), binding.revision.as_deref())
    .await
  {
    Ok(metadata) => metadata,
    Err(DropboxWriteError::Conflict { .. }) => {
      // Trust boundary: whoever can write the bound Dropbox path is a squad
      // collaborator, so their checkpoint merges without any further
      // authentication. The document-id gate below only prevents accidental
      // cross-document clobbering, not a malicious folder member.
      let remote = client
        .download(&binding.remote_path)
        .await
        .map_err(|error| io::Error::other(format!("Dropbox conflict download failed: {error:#}")))?;
      let local_package = flowstate_document::DocumentPackage::from_bytes(&package)?;
      let remote_package = flowstate_document::DocumentPackage::from_bytes(&remote.bytes)?;
      if local_package.manifest.document_id != remote_package.manifest.document_id {
        return Err(io::Error::new(
          io::ErrorKind::InvalidData,
          "Dropbox path contains a different Flowstate document; it was not overwritten",
        ));
      }
      let local_doc = local_package.load_loro_doc()?;
      let remote_doc = remote_package.load_loro_doc()?;
      let update = remote_doc
        .export(ExportMode::updates(&local_doc.oplog_vv()))
        .map_err(io::Error::other)?;
      if !update.is_empty() {
        io_handle
          .import_remote_update(update)
          .await
          .map_err(|error| io::Error::other(format!("merging Dropbox changes failed: {error:#}")))?;
      }
      io_handle
        .checkpoint_package(title.clone(), Some(local_path.to_path_buf()), flowstate_document::RevisionStamp::session())
        .await
        .map_err(|error| io::Error::other(format!("saving merged Dropbox checkpoint failed: {error:#}")))?;
      let merged = io_handle
        .package_bytes(title)
        .await
        .map_err(|error| io::Error::other(format!("assembling merged Dropbox checkpoint failed: {error:#}")))?;
      client
        .put_checkpoint(&binding.remote_path, merged, Some(&remote.metadata.rev))
        .await
        .map_err(|error| io::Error::other(format!("Dropbox changed again during conflict recovery: {:#}", error.into_anyhow())))?
    },
    Err(DropboxWriteError::Other(error)) => {
      return Err(io::Error::other(format!("saved locally, but Dropbox upload failed: {error:#}")));
    },
  };
  binding.revision = Some(metadata.rev);
  save_dropbox_document_binding(binding)?;

  // Persist a token refreshed during this request. The client never logs or
  // returns the bearer outside this settings update.
  let refreshed = client.credentials().await;
  save_dropbox_collaboration(refreshed, root, true)
}
