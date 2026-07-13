use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use thiserror::Error;
use wasmtime::component::{Component, Linker};
use wasmtime::{Config, Engine, Store, StoreLimitsBuilder};
use wasmtime_wasi::p2::pipe::MemoryOutputPipe;
use wasmtime_wasi::{DirPerms, FilePerms, WasiCtx, WasiCtxBuilder};

mod host_state;
use host_state::State;

wasmtime::component::bindgen!({ path: "wit", world: "extension" });

const OUTPUT_LIMIT: usize = 10 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    pub memory_limit: usize,
    pub output_limit: usize,
    pub allow_network: bool,
}

impl Default for RuntimeConfig {
    fn default() -> Self { Self { memory_limit: 1024 * 1024 * 1024, output_limit: OUTPUT_LIMIT, allow_network: true } }
}

#[derive(Clone, Debug)]
pub struct Invocation {
    pub component: PathBuf,
    pub extension_root: PathBuf,
    pub data_root: PathBuf,
    pub document_root: Option<PathBuf>,
    pub action_id: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct InvocationOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("{code}: {message}")]
pub struct HostError { pub code: String, pub message: String }

pub trait ExtensionHost: Send + 'static {
    fn snapshot(&mut self) -> Result<String, HostError>;
    fn selection(&mut self) -> Result<String, HostError>;
    fn apply_edits(&mut self, edits_json: &str) -> Result<String, HostError>;
    fn refresh_from_disk(&mut self) -> Result<String, HostError>;
    fn set_action_label(&mut self, action_id: &str, label: &str) -> Result<(), HostError>;
    fn set_status(&mut self, message: &str);
}

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("extension is already running")]
    AlreadyRunning,
    #[error("extension was cancelled")]
    Cancelled,
    #[error(transparent)]
    Wasmtime(#[from] anyhow::Error),
}

#[derive(Clone)]
pub struct CancellationHandle { cancelled: Arc<AtomicBool>, engine: Engine }

impl CancellationHandle {
    pub fn cancel(&self) { self.cancelled.store(true, Ordering::Release); self.engine.increment_epoch(); }
}

pub struct Runtime {
    engine: Engine,
    config: RuntimeConfig,
    running: Mutex<std::collections::HashSet<String>>,
}

impl Runtime {
    pub fn new(config: RuntimeConfig) -> Result<Self, RuntimeError> {
        let mut engine_config = Config::new();
        engine_config.wasm_component_model(true).epoch_interruption(true);
        Ok(Self { engine: Engine::new(&engine_config)?, config, running: Mutex::new(std::collections::HashSet::new()) })
    }

    pub fn cancellation_handle(&self) -> CancellationHandle {
        CancellationHandle { cancelled: Arc::new(AtomicBool::new(false)), engine: self.engine.clone() }
    }

    pub fn invoke<H: ExtensionHost>(&self, extension_id: &str, invocation: &Invocation, host: H, cancellation: &CancellationHandle) -> Result<InvocationOutput, RuntimeError> {
        let _guard = RunningGuard::acquire(&self.running, extension_id)?;
        if cancellation.cancelled.load(Ordering::Acquire) { return Err(RuntimeError::Cancelled); }
        let component = Component::from_file(&self.engine, &invocation.component)?;
        let stdout = MemoryOutputPipe::new(self.config.output_limit);
        let stderr = MemoryOutputPipe::new(self.config.output_limit);
        let wasi = build_wasi(invocation, &self.config, &stdout, &stderr)?;
        let limits = StoreLimitsBuilder::new().memory_size(self.config.memory_limit).build();
        let mut store = Store::new(&self.engine, State::new(wasi, limits, host));
        store.limiter(|state| &mut state.limits);
        store.set_epoch_deadline(1);
        let cancelled = Arc::clone(&cancellation.cancelled);
        store.epoch_deadline_callback(move |_| {
            if cancelled.load(Ordering::Acquire) {
                Err(anyhow::anyhow!("extension cancelled"))
            } else {
                Ok(wasmtime::UpdateDeadline::Continue(1))
            }
        });
        let mut linker = Linker::new(&self.engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;
        flowstate::extension::host::add_to_linker::<_, wasmtime::component::HasSelf<_>>(&mut linker, |state| state)?;
        let instance = Extension::instantiate(&mut store, &component, &linker)?;
        let result = instance.call_run(&mut store, &invocation.action_id)?;
        if cancellation.cancelled.load(Ordering::Acquire) { return Err(RuntimeError::Cancelled); }
        result.map_err(|message| RuntimeError::Wasmtime(anyhow::anyhow!(message)))?;
        Ok(InvocationOutput { stdout: stdout.contents().to_vec(), stderr: stderr.contents().to_vec() })
    }
}

fn build_wasi(
    invocation: &Invocation,
    config: &RuntimeConfig,
    stdout: &MemoryOutputPipe,
    stderr: &MemoryOutputPipe,
) -> anyhow::Result<WasiCtx> {
    std::fs::create_dir_all(&invocation.data_root)?;
    let mut builder = WasiCtxBuilder::new();
    builder.stdout(stdout.clone()).stderr(stderr.clone()).allow_blocking_current_thread(true);
    builder.preopened_dir(&invocation.extension_root, "/extension", DirPerms::READ, FilePerms::READ)?;
    builder.preopened_dir(&invocation.data_root, "/data", DirPerms::all(), FilePerms::all())?;
    if let Some(document_root) = &invocation.document_root {
        builder.preopened_dir(document_root, "/document", DirPerms::all(), FilePerms::all())?;
    }
    if config.allow_network { builder.inherit_network(); }
    Ok(builder.build())
}

struct RunningGuard<'a> {
    running: &'a Mutex<std::collections::HashSet<String>>,
    extension_id: String,
}

impl<'a> RunningGuard<'a> {
    fn acquire(running: &'a Mutex<std::collections::HashSet<String>>, extension_id: &str) -> Result<Self, RuntimeError> {
        if !running.lock().insert(extension_id.to_owned()) { return Err(RuntimeError::AlreadyRunning); }
        Ok(Self { running, extension_id: extension_id.to_owned() })
    }
}

impl Drop for RunningGuard<'_> {
    fn drop(&mut self) { self.running.lock().remove(&self.extension_id); }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_actions_per_extension() {
        let running = Mutex::new(std::collections::HashSet::new());
        let first = RunningGuard::acquire(&running, "com.example.one").unwrap();
        assert!(matches!(RunningGuard::acquire(&running, "com.example.one"), Err(RuntimeError::AlreadyRunning)));
        assert!(RunningGuard::acquire(&running, "com.example.two").is_ok());
        drop(first);
        assert!(RunningGuard::acquire(&running, "com.example.one").is_ok());
    }

    #[test]
    fn cancellation_is_token_scoped() {
        let runtime = Runtime::new(RuntimeConfig::default()).unwrap();
        let cancelled = runtime.cancellation_handle();
        let other = runtime.cancellation_handle();
        cancelled.cancel();
        assert!(cancelled.cancelled.load(Ordering::Acquire));
        assert!(!other.cancelled.load(Ordering::Acquire));
    }
}
