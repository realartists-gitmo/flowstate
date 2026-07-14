//! Dropbox-backed rendezvous sidecars and revision-conditional checkpoints.
//!
//! The API client intentionally has no opinion about trust or admission. It
//! stores signed discovery records and opaque document bytes; callers still
//! run [`crate::discovery::eligible_advertisements`] before showing peers.

use std::{
  future::Future,
  pin::Pin,
  sync::Arc,
  time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result, anyhow, bail, ensure};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use iroh::PublicKey;
use rand::RngCore as _;
use reqwest::{Client, Response, StatusCode, header};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tokio::sync::Mutex;
use url::Url;

use crate::discovery::{DiscoveryAdvertisement, DiscoveryTransport, RendezvousBackend};

const API_BASE: &str = "https://api.dropboxapi.com/2";
const CONTENT_BASE: &str = "https://content.dropboxapi.com/2";
const TOKEN_URL: &str = "https://api.dropboxapi.com/oauth2/token";
const API_ARG: &str = "Dropbox-API-Arg";
const API_RESULT: &str = "Dropbox-API-Result";
/// Large checkpoints upload/download in one request, so there is no overall
/// request deadline — only bounded connect and read-idle times, so a dead
/// connection can never hang a discovery refresh or checkpoint save forever.
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const HTTP_READ_TIMEOUT: Duration = Duration::from_secs(60);
/// Upper bound on a downloaded checkpoint/advertisement body. Snapshots are
/// zstd-compressed and orders of magnitude smaller than this in practice.
const MAX_DOWNLOAD_LEN: u64 = 256 * 1024 * 1024;

fn http_client() -> Client {
  Client::builder()
    .connect_timeout(HTTP_CONNECT_TIMEOUT)
    .read_timeout(HTTP_READ_TIMEOUT)
    .build()
    .unwrap_or_default()
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct DropboxCredentials {
  pub app_key: String,
  pub access_token: String,
  pub refresh_token: Option<String>,
  /// Unix seconds. `None` also supports manually generated development tokens.
  pub access_token_expires_at: Option<u64>,
}

/// Transient state for Dropbox's desktop OAuth code flow. The verifier and
/// state must be held only until the custom-URL callback arrives.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DropboxPkceFlow {
  pub authorization_url: Url,
  pub code_verifier: String,
  pub state: String,
  pub redirect_uri: String,
  pub app_key: String,
}

impl DropboxPkceFlow {
  pub fn begin(app_key: impl Into<String>, redirect_uri: impl Into<String>) -> Result<Self> {
    let app_key = app_key.into();
    let redirect_uri = redirect_uri.into();
    ensure!(!app_key.trim().is_empty(), "Dropbox app key is missing");
    let mut verifier_bytes = [0_u8; 64];
    rand::rng().fill_bytes(&mut verifier_bytes);
    let code_verifier = URL_SAFE_NO_PAD.encode(verifier_bytes);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(code_verifier.as_bytes()));
    let mut state_bytes = [0_u8; 24];
    rand::rng().fill_bytes(&mut state_bytes);
    let state = URL_SAFE_NO_PAD.encode(state_bytes);
    let mut authorization_url = Url::parse("https://www.dropbox.com/oauth2/authorize")?;
    authorization_url
      .query_pairs_mut()
      .append_pair("client_id", &app_key)
      .append_pair("response_type", "code")
      .append_pair("redirect_uri", &redirect_uri)
      .append_pair("code_challenge_method", "S256")
      .append_pair("code_challenge", &challenge)
      .append_pair("token_access_type", "offline")
      .append_pair("state", &state);
    Ok(Self {
      authorization_url,
      code_verifier,
      state,
      redirect_uri,
      app_key,
    })
  }

  pub fn verify_callback(&self, callback: &Url) -> Result<String> {
    let query: std::collections::HashMap<_, _> = callback.query_pairs().collect();
    ensure!(
      query.get("state").is_some_and(|state| state == &self.state),
      "Dropbox authorization state did not match"
    );
    if let Some(error) = query
      .get("error_description")
      .or_else(|| query.get("error"))
    {
      bail!("Dropbox authorization was declined: {error}");
    }
    query
      .get("code")
      .map(|code| code.to_string())
      .context("Dropbox authorization callback omitted its code")
  }

  pub async fn exchange(&self, callback: &Url) -> Result<DropboxCredentials> {
    let code = self.verify_callback(callback)?;
    let response = http_client()
      .post(TOKEN_URL)
      .form(&[
        ("grant_type", "authorization_code"),
        ("code", code.as_str()),
        ("client_id", self.app_key.as_str()),
        ("code_verifier", self.code_verifier.as_str()),
        ("redirect_uri", self.redirect_uri.as_str()),
      ])
      .send()
      .await
      .context("exchanging Dropbox authorization code")?;
    let response = checked(response, "OAuth code exchange").await?;
    let token: TokenResponse = response
      .json()
      .await
      .context("decoding Dropbox authorization response")?;
    Ok(DropboxCredentials {
      app_key: self.app_key.clone(),
      access_token: token.access_token,
      refresh_token: token.refresh_token,
      access_token_expires_at: token
        .expires_in
        .map(|seconds| unix_now().saturating_add(seconds)),
    })
  }
}

#[derive(Clone)]
pub struct DropboxClient {
  http: Client,
  credentials: Arc<Mutex<DropboxCredentials>>,
  api_base: Arc<str>,
  content_base: Arc<str>,
  token_url: Arc<str>,
}

impl DropboxClient {
  #[must_use]
  pub fn new(credentials: DropboxCredentials) -> Self {
    Self::with_endpoints(credentials, API_BASE, CONTENT_BASE, TOKEN_URL)
  }

  /// Endpoint injection is useful for deterministic integration tests and for
  /// Dropbox-compatible enterprise gateways.
  #[must_use]
  pub fn with_endpoints(credentials: DropboxCredentials, api_base: &str, content_base: &str, token_url: &str) -> Self {
    Self {
      http: http_client(),
      credentials: Arc::new(Mutex::new(credentials)),
      api_base: api_base.trim_end_matches('/').into(),
      content_base: content_base.trim_end_matches('/').into(),
      token_url: token_url.into(),
    }
  }

  pub async fn credentials(&self) -> DropboxCredentials {
    self.credentials.lock().await.clone()
  }

  async fn bearer_token(&self) -> Result<String> {
    let mut credentials = self.credentials.lock().await;
    ensure!(!credentials.access_token.is_empty(), "Dropbox is not connected");
    let now = unix_now();
    let should_refresh = credentials
      .access_token_expires_at
      .is_some_and(|expires| expires <= now.saturating_add(60));
    if !should_refresh {
      return Ok(credentials.access_token.clone());
    }
    let refresh_token = credentials
      .refresh_token
      .as_deref()
      .context("Dropbox access expired and no refresh token is available")?;
    ensure!(!credentials.app_key.is_empty(), "Dropbox app key is missing");
    let response = self
      .http
      .post(self.token_url.as_ref())
      .form(&[
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", credentials.app_key.as_str()),
      ])
      .send()
      .await
      .context("refreshing Dropbox access token")?;
    let response = checked(response, "refreshing Dropbox access token").await?;
    let token: TokenResponse = response
      .json()
      .await
      .context("decoding Dropbox token response")?;
    credentials.access_token = token.access_token.clone();
    credentials.access_token_expires_at = token.expires_in.map(|seconds| now.saturating_add(seconds));
    drop(credentials);
    Ok(token.access_token)
  }

  async fn rpc(&self, route: &str, body: Value) -> Result<Response> {
    let token = self.bearer_token().await?;
    let response = self
      .http
      .post(format!("{}{route}", self.api_base))
      .bearer_auth(token)
      .json(&body)
      .send()
      .await
      .with_context(|| format!("calling Dropbox {route}"))?;
    checked(response, route).await
  }

  async fn upload(&self, path: &str, bytes: Vec<u8>, mode: Value) -> Result<DropboxFileMetadata, DropboxWriteError> {
    let token = self
      .bearer_token()
      .await
      .map_err(DropboxWriteError::Other)?;
    let argument = json!({
      "path": path,
      "mode": mode,
      "autorename": false,
      "mute": true,
      "strict_conflict": true
    });
    let response = self
      .http
      .post(format!("{}/files/upload", self.content_base))
      .bearer_auth(token)
      .header(API_ARG, argument.to_string())
      .header(header::CONTENT_TYPE, "application/octet-stream")
      .body(bytes)
      .send()
      .await
      .map_err(|error| DropboxWriteError::Other(error.into()))?;
    if response.status() == StatusCode::CONFLICT {
      return Err(DropboxWriteError::Conflict { current_revision: None });
    }
    let response = checked(response, "files/upload")
      .await
      .map_err(DropboxWriteError::Other)?;
    response
      .json()
      .await
      .context("decoding Dropbox upload metadata")
      .map_err(DropboxWriteError::Other)
  }

  pub async fn download(&self, path: &str) -> Result<DropboxDownloadedFile> {
    let token = self.bearer_token().await?;
    let response = self
      .http
      .post(format!("{}/files/download", self.content_base))
      .bearer_auth(token)
      .header(API_ARG, json!({ "path": path }).to_string())
      .send()
      .await
      .context("downloading Dropbox file")?;
    let response = checked(response, "files/download").await?;
    let metadata = response
      .headers()
      .get(API_RESULT)
      .context("Dropbox download omitted file metadata")?
      .to_str()
      .context("Dropbox download metadata is not UTF-8")?;
    let metadata: DropboxFileMetadata = serde_json::from_str(metadata).context("decoding Dropbox download metadata")?;
    ensure!(
      response.content_length().unwrap_or(0) <= MAX_DOWNLOAD_LEN,
      "Dropbox download exceeds the {MAX_DOWNLOAD_LEN}-byte limit"
    );
    let mut response = response;
    let mut bytes = Vec::new();
    while let Some(chunk) = response
      .chunk()
      .await
      .context("reading Dropbox download body")?
    {
      ensure!(
        (bytes.len() as u64).saturating_add(chunk.len() as u64) <= MAX_DOWNLOAD_LEN,
        "Dropbox download exceeds the {MAX_DOWNLOAD_LEN}-byte limit"
      );
      bytes.extend_from_slice(&chunk);
    }
    Ok(DropboxDownloadedFile { metadata, bytes })
  }

  async fn ensure_folder(&self, path: &str) -> Result<()> {
    let response = self
      .rpc("/files/create_folder_v2", json!({ "path": path, "autorename": false }))
      .await;
    match response {
      Ok(_) => Ok(()),
      Err(error) if dropbox_conflict(&error) => Ok(()),
      Err(error) => Err(error),
    }
  }

  async fn ensure_folder_tree(&self, path: &str) -> Result<()> {
    let mut current = String::new();
    for component in path.split('/').filter(|component| !component.is_empty()) {
      current.push('/');
      current.push_str(component);
      self.ensure_folder(&current).await?;
    }
    Ok(())
  }

  async fn list_folder(&self, path: &str) -> Result<Vec<DropboxEntry>> {
    let mut page: ListFolderResponse = self
      .rpc(
        "/files/list_folder",
        json!({ "path": path, "recursive": false, "include_deleted": false }),
      )
      .await?
      .json()
      .await
      .context("decoding Dropbox folder listing")?;
    let mut entries = page.entries;
    while page.has_more {
      page = self
        .rpc("/files/list_folder/continue", json!({ "cursor": page.cursor }))
        .await?
        .json()
        .await
        .context("decoding continued Dropbox folder listing")?;
      entries.append(&mut page.entries);
    }
    Ok(entries)
  }

  async fn delete(&self, path: &str) -> Result<()> {
    let response = self.rpc("/files/delete_v2", json!({ "path": path })).await;
    match response {
      Ok(_) => Ok(()),
      Err(error) if dropbox_not_found(&error) => Ok(()),
      Err(error) => Err(error),
    }
  }

  pub async fn metadata(&self, path: &str) -> Result<DropboxFileMetadata> {
    self
      .rpc("/files/get_metadata", json!({ "path": path, "include_deleted": false }))
      .await?
      .json()
      .await
      .context("decoding Dropbox file metadata")
  }

  /// Upload a package only if Dropbox still has the revision the caller read.
  /// `None` is a create-only write and therefore cannot overwrite an unknown
  /// remote document.
  pub async fn put_checkpoint(
    &self,
    path: &str,
    package: Vec<u8>,
    expected_revision: Option<&str>,
  ) -> Result<DropboxFileMetadata, DropboxWriteError> {
    let mode = expected_revision.map_or_else(|| json!({ ".tag": "add" }), |rev| json!({ ".tag": "update", "update": rev }));
    match self.upload(path, package, mode).await {
      Err(DropboxWriteError::Conflict { .. }) => {
        let current_revision = self.metadata(path).await.ok().map(|metadata| metadata.rev);
        Err(DropboxWriteError::Conflict { current_revision })
      },
      result => result,
    }
  }
}

#[derive(Clone)]
pub struct DropboxRendezvousBackend {
  client: DropboxClient,
  root: Arc<str>,
}

impl DropboxRendezvousBackend {
  #[must_use]
  pub fn new(client: DropboxClient, root: impl Into<String>) -> Self {
    let root = normalized_root(&root.into());
    Self { client, root: root.into() }
  }

  fn document_folder(&self, fingerprint: &[u8; 32]) -> String {
    format!("{}/{}/{}", self.root, "presence", hex(fingerprint))
  }

  fn advertisement_path(&self, identity: &PublicKey, device_id: u128, fingerprint: &[u8; 32]) -> String {
    let identity_hash = blake3::hash(&postcard::to_stdvec(identity).expect("public key should serialize"));
    format!(
      "{}/{}-{device_id:032x}.ad",
      self.document_folder(fingerprint),
      hex(identity_hash.as_bytes())
    )
  }
}

impl RendezvousBackend for DropboxRendezvousBackend {
  fn transport(&self) -> DiscoveryTransport {
    DiscoveryTransport::Dropbox
  }

  fn publish(&self, advertisement: DiscoveryAdvertisement) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
    Box::pin(async move {
      let folder = self.document_folder(&advertisement.document_fingerprint);
      self.client.ensure_folder_tree(&folder).await?;
      let path = self.advertisement_path(&advertisement.identity, advertisement.device_id, &advertisement.document_fingerprint);
      let bytes = postcard::to_stdvec(&advertisement).context("encoding Dropbox discovery advertisement")?;
      self
        .client
        .upload(&path, bytes, json!({ ".tag": "overwrite" }))
        .await
        .map_err(DropboxWriteError::into_anyhow)?;
      Ok(())
    })
  }

  fn scan(&self, document_fingerprint: [u8; 32]) -> Pin<Box<dyn Future<Output = Result<Vec<DiscoveryAdvertisement>>> + Send + '_>> {
    Box::pin(async move {
      let folder = self.document_folder(&document_fingerprint);
      let entries = match self.client.list_folder(&folder).await {
        Ok(entries) => entries,
        Err(error) if dropbox_not_found(&error) => return Ok(Vec::new()),
        Err(error) => return Err(error),
      };
      let mut advertisements = Vec::new();
      for entry in entries {
        let is_sidecar = std::path::Path::new(&entry.name)
          .extension()
          .is_some_and(|extension| extension.eq_ignore_ascii_case("ad"));
        if entry.tag != "file" || !is_sidecar {
          continue;
        }
        let Some(path) = entry.path_lower.as_deref() else { continue };
        match self.client.download(path).await {
          Ok(file) => match postcard::from_bytes::<DiscoveryAdvertisement>(&file.bytes) {
            Ok(advertisement) => advertisements.push(advertisement),
            Err(error) => tracing::warn!(%error, %path, "ignoring malformed Dropbox discovery sidecar"),
          },
          Err(error) => tracing::warn!(%error, %path, "failed to fetch Dropbox discovery sidecar"),
        }
      }
      Ok(advertisements)
    })
  }

  fn clear(
    &self,
    identity: PublicKey,
    device_id: u128,
    document_fingerprint: [u8; 32],
  ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
    Box::pin(async move {
      self
        .client
        .delete(&self.advertisement_path(&identity, device_id, &document_fingerprint))
        .await
    })
  }
}

#[derive(Clone, Debug, Deserialize)]
struct TokenResponse {
  access_token: String,
  expires_in: Option<u64>,
  refresh_token: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DropboxFileMetadata {
  pub id: String,
  pub name: String,
  pub rev: String,
  pub path_lower: Option<String>,
  pub size: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct DropboxDownloadedFile {
  pub metadata: DropboxFileMetadata,
  pub bytes: Vec<u8>,
}

#[derive(Debug)]
pub enum DropboxWriteError {
  Conflict { current_revision: Option<String> },
  Other(anyhow::Error),
}

impl DropboxWriteError {
  #[must_use]
  pub fn into_anyhow(self) -> anyhow::Error {
    match self {
      Self::Conflict { current_revision } => current_revision.map_or_else(
        || anyhow!("Dropbox file changed since it was read"),
        |revision| anyhow!("Dropbox file changed since it was read (current revision: {revision})"),
      ),
      Self::Other(error) => error,
    }
  }
}

#[derive(Debug, Deserialize)]
struct ListFolderResponse {
  entries: Vec<DropboxEntry>,
  cursor: String,
  has_more: bool,
}

#[derive(Debug, Deserialize)]
struct DropboxEntry {
  #[serde(rename = ".tag")]
  tag: String,
  name: String,
  path_lower: Option<String>,
}

async fn checked(response: Response, operation: &str) -> Result<Response> {
  if response.status().is_success() {
    return Ok(response);
  }
  let status = response.status();
  let body = response.text().await.unwrap_or_default();
  bail!("Dropbox {operation} failed ({status}): {body}")
}

fn dropbox_conflict(error: &anyhow::Error) -> bool {
  error.to_string().contains("(409 Conflict)")
}

fn dropbox_not_found(error: &anyhow::Error) -> bool {
  let message = error.to_string();
  message.contains("not_found") || message.contains("path/not_found")
}

fn normalized_root(root: &str) -> String {
  let root = root.trim().trim_matches('/');
  if root.is_empty() {
    "/.flowstate".into()
  } else {
    format!("/{root}/.flowstate")
  }
}

fn hex(bytes: &[u8]) -> String {
  const DIGITS: &[u8; 16] = b"0123456789abcdef";
  let mut value = String::with_capacity(bytes.len() * 2);
  for byte in bytes {
    value.push(char::from(DIGITS[usize::from(byte >> 4)]));
    value.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
  }
  value
}

fn unix_now() -> u64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .unwrap_or(Duration::ZERO)
    .as_secs()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn roots_and_sidecar_names_are_stable_and_dropbox_safe() {
    let client = DropboxClient::new(DropboxCredentials::default());
    let backend = DropboxRendezvousBackend::new(client, "/Debates/");
    let identity = crate::identity::PortableIdentitySecret::generate().public();
    let path = backend.advertisement_path(&identity, 7, &[0xab; 32]);
    assert!(path.starts_with("/Debates/.flowstate/presence/abab"));
    assert!(path.ends_with("-00000000000000000000000000000007.ad"));
    assert!(!path.contains(' '));
  }

  #[test]
  fn empty_root_uses_app_folder() {
    assert_eq!(normalized_root(""), "/.flowstate");
    assert_eq!(normalized_root("/Team"), "/Team/.flowstate");
  }

  #[test]
  fn pkce_uses_s256_offline_tokens_and_validates_state() {
    let flow = DropboxPkceFlow::begin("app-key", "flowstate://oauth/dropbox").unwrap();
    let query: std::collections::HashMap<_, _> = flow.authorization_url.query_pairs().collect();
    assert_eq!(
      query
        .get("code_challenge_method")
        .map(std::borrow::Cow::as_ref),
      Some("S256")
    );
    assert_eq!(query.get("token_access_type").map(std::borrow::Cow::as_ref), Some("offline"));
    let callback = Url::parse(&format!("flowstate://oauth/dropbox?code=abc&state={}", flow.state)).unwrap();
    assert_eq!(flow.verify_callback(&callback).unwrap(), "abc");
    assert!(
      flow
        .verify_callback(&Url::parse("flowstate://oauth/dropbox?code=abc&state=wrong").unwrap())
        .is_err()
    );
  }
}
