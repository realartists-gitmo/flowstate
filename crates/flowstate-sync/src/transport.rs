use super::*;

pub(crate) async fn write_frame(send: &mut SendStream, bytes: &[u8], max_message_bytes: usize) -> AnyResult<()> {
  ensure!(
    bytes.len() <= max_message_bytes,
    SyncError::FrameTooLarge {
      len: bytes.len(),
      max: max_message_bytes,
    }
  );
  let len = u32::try_from(bytes.len()).context("Flowstate frame length exceeds u32")?;
  send
    .write_all(&len.to_le_bytes())
    .await
    .context("failed to write Flowstate frame length")?;
  send
    .write_all(bytes)
    .await
    .context("failed to write Flowstate frame payload")
}

pub(crate) async fn read_frame(recv: &mut RecvStream, max_message_bytes: usize) -> AnyResult<Vec<u8>> {
  let mut len = [0; 4];
  recv
    .read_exact(&mut len)
    .await
    .context("failed to read Flowstate frame length")?;
  let len = u32::from_le_bytes(len) as usize;
  ensure!(len <= max_message_bytes, SyncError::FrameTooLarge { len, max: max_message_bytes });
  let mut bytes = vec![0; len];
  recv
    .read_exact(&mut bytes)
    .await
    .context("failed to read Flowstate frame payload")?;
  Ok(bytes)
}

pub(crate) fn validate_hello(hello: &HelloMessage, config: &FlowstateSyncConfig) -> AnyResult<()> {
  ensure!(hello.protocol_version == FLOWSTATE_PROTOCOL_VERSION, SyncError::ProtocolMismatch);
  ensure!(hello.collab_schema == COLLAB_SCHEMA_VERSION, SyncError::ProtocolMismatch);
  ensure!(hello.crdt_engine == "loro", SyncError::ProtocolMismatch);
  ensure!(hello.document_id == config.document_id, SyncError::ProtocolMismatch);
  ensure!(hello.format_kind == config.format_kind, SyncError::ProtocolMismatch);
  Ok(())
}
