//! Explicit provider selection for local processes and recorded adapter fixtures.
use crate::{model::SampleError, provider::SampleProvider, registry::ProviderSpec};
use std::path::{Path, PathBuf};

/// What a search needs from the process that hosts provider plugins.
pub trait ProviderRuntime: Sync {
    fn is_installed(&self, spec: &ProviderSpec) -> bool;
    fn open(
        &self,
        spec: &ProviderSpec,
        key: Option<&str>,
    ) -> Result<Box<dyn SampleProvider>, SampleError>;
}

/// The production runtime always uses the local plugin protocol.
/// An explicit directory keeps tests independent of process configuration.
pub struct LocalRuntime {
    directory: Option<PathBuf>,
}
impl LocalRuntime {
    /// Use an explicit plugin directory instead of the process configuration.
    pub fn from_directory(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: Some(directory.into()),
        }
    }
    /// Use `OPENCOMPOSE_SAMPLE_PLUGIN_DIR` or the user's provider directory.
    pub fn from_process_configuration() -> Self {
        let directory = std::env::var_os("OPENCOMPOSE_SAMPLE_PLUGIN_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .or_else(|| std::env::var_os("USERPROFILE"))
                    .map(|home| PathBuf::from(home).join(".opencompose/sample-providers"))
            });
        Self { directory }
    }
    pub(crate) fn directory(&self) -> Option<&Path> {
        self.directory.as_deref()
    }
}
impl ProviderRuntime for LocalRuntime {
    fn is_installed(&self, spec: &ProviderSpec) -> bool {
        crate::local_plugin::is_installed_in(self.directory(), spec.id)
    }
    fn open(
        &self,
        spec: &ProviderSpec,
        key: Option<&str>,
    ) -> Result<Box<dyn SampleProvider>, SampleError> {
        crate::local_plugin::open_in(self.directory(), spec, key)
    }
}

/// Recorded adapter tests opt into this runtime explicitly. Enabling the
/// feature never changes selection in the application or production API.
#[cfg(any(test, feature = "test-providers"))]
pub struct FixtureBuiltinRuntime;
#[cfg(any(test, feature = "test-providers"))]
impl ProviderRuntime for FixtureBuiltinRuntime {
    fn is_installed(&self, spec: &ProviderSpec) -> bool {
        matches!(
            spec.id,
            "wikimedia-commons" | "openverse" | "internet-archive" | "freesound" | "jamendo"
        )
    }
    fn open(
        &self,
        spec: &ProviderSpec,
        key: Option<&str>,
    ) -> Result<Box<dyn SampleProvider>, SampleError> {
        crate::builtin_provider(spec, key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        provider_for_with,
        registry::{self, ProviderKeys},
    };
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TestDirectory(PathBuf);
    impl TestDirectory {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "oc-sample-runtime-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn an_empty_local_root_never_implicitly_installs_fixture_providers() {
        let directory = TestDirectory::new();
        let runtime = LocalRuntime::from_directory(&directory.0);
        let keys = ProviderKeys::default();
        for id in ["commons", "openverse", "ia", "freesound", "jamendo"] {
            let spec = registry::spec(id).unwrap();
            assert!(!runtime.is_installed(spec));
            let row = registry::availability_of_with(spec, keys, &runtime);
            assert!(!row.available);
            assert!(
                row.reason
                    .unwrap()
                    .contains("no local sample-search plugin")
            );
            let error = provider_for_with(Some(id), keys, &runtime).err().unwrap();
            assert_eq!(error.code(), Some("provider_unavailable"));
        }
        assert!(registry::available_ids_with(keys, &runtime).is_empty());
        assert!(registry::default_search_ids_with(keys, &runtime).is_empty());
        assert!(FixtureBuiltinRuntime.is_installed(registry::spec("commons").unwrap()));
        assert!(!runtime.is_installed(registry::spec("commons").unwrap()));
    }

    #[cfg(debug_assertions)]
    mod process_protocol {
        use super::*;
        use crate::model::{Quality, SampleItem, SampleKind, SamplePage, SampleQuery};
        use crate::provider::{FetchResponse, SampleFetch};
        use serde_json::{Value, json};

        struct NoNetwork;
        impl SampleFetch for NoNetwork {
            fn get(&self, _: &str, _: usize) -> Result<FetchResponse, String> {
                panic!("the process fixture must not reach the network")
            }
        }

        fn fixture(reply: &str) -> (TestDirectory, LocalRuntime) {
            let directory = TestDirectory::new();
            let request = directory.0.join("request.json");
            #[cfg(unix)]
            let (executable, args) = {
                fn quote(text: &str) -> String {
                    format!("'{}'", text.replace('\'', "'\\''"))
                }
                let script = directory.0.join("provider.sh");
                std::fs::write(
                    &script,
                    format!(
                        "cat > {}\nprintf '%s' {}\n",
                        quote(request.to_str().unwrap()),
                        quote(reply)
                    ),
                )
                .unwrap();
                (
                    PathBuf::from("/bin/sh"),
                    vec![script.to_string_lossy().into_owned()],
                )
            };
            #[cfg(windows)]
            let (executable, args) = {
                fn quote(text: &str) -> String {
                    format!("'{}'", text.replace('\'', "''"))
                }
                let script = directory.0.join("provider.ps1");
                std::fs::write(&script, format!("[System.IO.File]::WriteAllText({}, [Console]::In.ReadToEnd()); [Console]::Out.Write({})", quote(request.to_str().unwrap()), quote(reply))).unwrap();
                let executable = PathBuf::from(
                    std::env::var_os("SystemRoot").expect("Windows system directory"),
                )
                .join("System32/WindowsPowerShell/v1.0/powershell.exe");
                (
                    executable,
                    vec![
                        "-NoProfile".into(),
                        "-NonInteractive".into(),
                        "-ExecutionPolicy".into(),
                        "Bypass".into(),
                        "-File".into(),
                        script.to_string_lossy().into_owned(),
                    ],
                )
            };
            std::fs::write(directory.0.join("wikimedia-commons.json"), serde_json::to_vec(&json!({
                "protocol": 1, "executable": executable, "args": args, "configuration": {"fixture": true}
            })).unwrap()).unwrap();
            let runtime = LocalRuntime::from_directory(&directory.0);
            (directory, runtime)
        }

        fn item() -> SampleItem {
            SampleItem {
                id: "commons:fixture".into(),
                provider: "wikimedia-commons".into(),
                name: "Fixture".into(),
                kind: SampleKind::Unknown,
                duration_seconds: Some(0.1),
                channels: Some(1),
                sample_rate: Some(48000),
                license: crate::license::License::cc0(),
                attribution: "Fixture".into(),
                source_url: "https://example.test/source".into(),
                preview_url: None,
                download_url: "https://example.test/audio.wav".into(),
                sha256: None,
                bytes: None,
                quality: Quality::Original,
                tags: vec![],
            }
        }

        #[test]
        fn real_local_process_search_and_resolve_use_the_same_protocol_as_production() {
            let page = SamplePage {
                provider: "wikimedia-commons".into(),
                offset: 0,
                limit: 1,
                total: Some(1),
                next_offset: None,
                items: vec![item()],
            };
            let (directory, runtime) = fixture(&json!({"status": "ok", "value": page}).to_string());
            let spec = registry::spec("commons").unwrap();
            assert!(
                registry::availability_of_with(spec, ProviderKeys::default(), &runtime).available
            );
            let provider = provider_for_with(None, ProviderKeys::default(), &runtime).unwrap();
            let query = SampleQuery {
                text: "fixture".into(),
                limit: 1,
                ..Default::default()
            };
            assert_eq!(provider.search(&query, &NoNetwork).unwrap(), page);
            let request: Value =
                serde_json::from_slice(&std::fs::read(directory.0.join("request.json")).unwrap())
                    .unwrap();
            assert_eq!(request["protocol"], 1);
            assert_eq!(request["operation"]["entrypoint"], "search");
            assert_eq!(request["operation"]["query"]["text"], "fixture");
            assert_eq!(request["configuration"]["fixture"], true);
            assert!(request["credential"].is_null());

            let (directory, runtime) =
                fixture(&json!({"status": "ok", "value": item()}).to_string());
            let provider = provider_for_with(None, ProviderKeys::default(), &runtime).unwrap();
            assert_eq!(
                provider.resolve("commons:fixture", &NoNetwork).unwrap(),
                item()
            );
            let request: Value =
                serde_json::from_slice(&std::fs::read(directory.0.join("request.json")).unwrap())
                    .unwrap();
            assert_eq!(request["operation"]["entrypoint"], "resolve");
            assert_eq!(request["operation"]["id"], "commons:fixture");
        }

        #[test]
        fn local_process_protocol_refuses_malformed_and_mismatched_results() {
            for reply in ["not json".to_string(), json!({"status":"ok","value":{"provider":"other", "offset":0,"limit":1,"total":null,"next_offset":null,"items":[]}}).to_string()] {
                let (_directory, runtime) = fixture(&reply);
                let provider = provider_for_with(None, ProviderKeys::default(), &runtime).unwrap();
                assert!(provider.search(&SampleQuery::default(), &NoNetwork).is_err());
            }
            let (_directory, runtime) = fixture(
                &json!({"status":"error", "code":"no_download", "message":"fixture refusal"})
                    .to_string(),
            );
            let provider = provider_for_with(None, ProviderKeys::default(), &runtime).unwrap();
            assert_eq!(
                provider
                    .resolve("commons:fixture", &NoNetwork)
                    .unwrap_err()
                    .code(),
                Some("no_download")
            );
        }
    }
}
