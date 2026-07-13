#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use flowstate_extension::{DirectoryAccess, DirectoryGrantResponse, ExtensionHost, HostError, Invocation, Runtime, RuntimeConfig};

    struct Host { status: Arc<Mutex<Vec<String>>> }

    impl ExtensionHost for Host {
        fn snapshot(&mut self) -> Result<String, HostError> { Ok("{}".to_owned()) }
        fn selection(&mut self) -> Result<String, HostError> { Ok("{}".to_owned()) }
        fn apply_edits(&mut self, _: &str) -> Result<String, HostError> { Ok("{}".to_owned()) }
        fn refresh_from_disk(&mut self) -> Result<String, HostError> { Ok("{}".to_owned()) }
        fn set_action_label(&mut self, _: &str, _: &str) -> Result<(), HostError> { Ok(()) }
        fn set_status(&mut self, message: &str) { self.status.lock().unwrap().push(message.to_owned()); }
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
        let output = runtime.invoke("com.example.test", &invocation, Host { status: Arc::clone(&status) }, &cancellation).unwrap();
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
        assert!(invocation.data_root.is_dir());
        assert_eq!(*status.lock().unwrap(), ["called"]);
    }
}
