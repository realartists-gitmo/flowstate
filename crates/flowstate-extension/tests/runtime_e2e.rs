#[cfg(test)]
mod tests {
    use std::sync::{Arc, Condvar, Mutex};
    use flowstate_extension::{DirectoryAccess, DirectoryGrantResponse, ExtensionHost, HostError, Invocation, Runtime, RuntimeConfig, RuntimeError};

    struct Host { status: Arc<Mutex<Vec<String>>>, signal: Option<Arc<(Mutex<bool>, Condvar)>> }

    impl ExtensionHost for Host {
        fn snapshot(&mut self) -> Result<String, HostError> { Ok("{}".to_owned()) }
        fn selection(&mut self) -> Result<String, HostError> { Ok("{}".to_owned()) }
        fn apply_edits(&mut self, _: &str) -> Result<String, HostError> { Ok("{}".to_owned()) }
        fn refresh_from_disk(&mut self) -> Result<String, HostError> { Ok("{}".to_owned()) }
        fn set_action_label(&mut self, _: &str, _: &str) -> Result<(), HostError> { Ok(()) }
        fn set_status(&mut self, message: &str) {
            self.status.lock().unwrap().push(message.to_owned());
            if let Some(signal) = &self.signal {
                *signal.0.lock().unwrap() = true;
                signal.1.notify_one();
            }
        }
        fn request_directory_access(&mut self, _: DirectoryAccess, _: Option<&str>) -> Result<DirectoryGrantResponse, HostError> {
            Err(HostError { code: "denied".to_owned(), message: "not used".to_owned() })
        }
    }

    #[test]
    fn invokes_component_export_end_to_end() {
        let directory = tempfile::tempdir().unwrap();
        let component = directory.path().join("extension.wat");
        std::fs::write(&component, include_str!("fixtures/success.wat")).unwrap();
        let invocation = Invocation {
            component,
            extension_root: directory.path().to_path_buf(),
            data_root: directory.path().join("data"),
            document_root: None,
            action_id: "run".to_owned(),
            directory_grants: Vec::new(),
        };
        let runtime = Runtime::new(RuntimeConfig { allow_network: false, ..RuntimeConfig::default() }).unwrap();
        let cancellation = runtime.cancellation_handle();
        let status = Arc::new(Mutex::new(Vec::new()));
        let output = runtime.invoke("com.example.test", &invocation, Host { status: Arc::clone(&status), signal: None }, &cancellation).unwrap();
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
        assert!(invocation.data_root.is_dir());
        assert_eq!(*status.lock().unwrap(), ["called"]);
    }

    #[test]
    fn network_enabled_runtime_also_links_base_wasi() {
        let directory = tempfile::tempdir().unwrap();
        let component = directory.path().join("wasi-environment.wat");
        std::fs::write(&component, include_str!("fixtures/wasi_environment.wat")).unwrap();
        let invocation = Invocation {
            component,
            extension_root: directory.path().to_path_buf(),
            data_root: directory.path().join("data"),
            document_root: None,
            action_id: "run".to_owned(),
            directory_grants: Vec::new(),
        };
        let runtime = Runtime::new(RuntimeConfig { allow_network: true, ..RuntimeConfig::default() }).unwrap();
        let cancellation = runtime.cancellation_handle();
        let status = Arc::new(Mutex::new(Vec::new()));

        runtime.invoke("com.example.wasi", &invocation, Host { status: Arc::clone(&status), signal: None }, &cancellation).unwrap();

        assert_eq!(*status.lock().unwrap(), ["called"]);
    }

    #[test]
    fn cancels_a_running_component() {
        let directory = tempfile::tempdir().unwrap();
        let component = directory.path().join("infinite.wat");
        let source = include_str!("fixtures/success.wat").replace(
            "i32.const 2048\n      i32.const 0\n      i32.store\n      i32.const 2048",
            "(loop $forever br $forever)\n      unreachable",
        );
        std::fs::write(&component, source).unwrap();
        let invocation = Invocation { component, extension_root: directory.path().into(), data_root: directory.path().join("data"), document_root: None, action_id: "run".into(), directory_grants: Vec::new() };
        let runtime = Arc::new(Runtime::new(RuntimeConfig { allow_network: false, ..RuntimeConfig::default() }).unwrap());
        let cancellation = runtime.cancellation_handle();
        let signal = Arc::new((Mutex::new(false), Condvar::new()));
        let thread_runtime = Arc::clone(&runtime);
        let thread_cancellation = cancellation.clone();
        let thread_signal = Arc::clone(&signal);
        let worker = std::thread::spawn(move || thread_runtime.invoke("com.example.infinite", &invocation, Host { status: Arc::new(Mutex::new(Vec::new())), signal: Some(thread_signal) }, &thread_cancellation));
        let mut started = signal.0.lock().unwrap();
        while !*started { started = signal.1.wait(started).unwrap(); }
        drop(started);
        cancellation.cancel();
        assert!(matches!(worker.join().unwrap(), Err(RuntimeError::Cancelled)));
    }
}
