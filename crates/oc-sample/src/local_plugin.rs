//! Local process protocol for sample search plugins.

use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
/// How a local provider helper resolves its outbound proxy.
pub enum Proxy {
    #[default]
    System,
    Manual(String),
    None,
}

#[derive(Clone, Deserialize, Serialize)]
/// Proxy and timeout a helper must use for one request.
pub struct Network {
    pub proxy: Proxy,
    pub timeout_seconds: u64,
}

impl Default for Network {
    fn default() -> Self {
        Self {
            proxy: Proxy::System,
            timeout_seconds: 15,
        }
    }
}

pub(crate) const UNAVAILABLE: &str = "no local sample-search plugin is installed";

/// Whether a local provider plugin with this id has a manifest.
pub fn is_installed(id: &str) -> bool {
    let runtime = crate::runtime::LocalRuntime::from_process_configuration();
    is_installed_in(runtime.directory(), id)
}

pub(crate) fn is_installed_in(directory: Option<&std::path::Path>, id: &str) -> bool {
    host::manifest(directory, id).is_some()
}

mod wire {
    use super::Network;
    use crate::model::SampleQuery;
    use serde::{Deserialize, Serialize};

    /// Protocol version written into every helper request.
    pub const VERSION: u32 = 1;

    #[derive(Deserialize, Serialize)]
    #[serde(tag = "entrypoint", rename_all = "snake_case")]
    /// One request the helper can perform.
    pub enum Operation {
        Search { query: SampleQuery },
        Resolve { id: String },
    }

    #[derive(Deserialize, Serialize)]
    /// A helper invocation: protocol, credentials, network and operation.
    pub struct Request {
        pub protocol: u32,
        pub credential: Option<String>,
        pub network: Network,
        pub configuration: serde_json::Value,
        pub operation: Operation,
    }

    #[derive(Deserialize, Serialize)]
    #[serde(tag = "status", rename_all = "snake_case")]
    /// The helper's tagged success or error response.
    pub enum Reply<T> {
        Ok {
            value: T,
        },
        Error {
            code: Option<String>,
            message: String,
        },
    }
}

mod host {
    use super::wire::{self, Operation, Reply, Request};
    use crate::model::{SampleError, SampleItem, SamplePage, SampleQuery};
    use crate::provider::{MAX_JSON_BYTES, SampleFetch, SampleProvider};
    use crate::registry::ProviderSpec;
    use serde::{Deserialize, de::DeserializeOwned};
    use std::io::{Read, Write};
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    #[derive(Deserialize)]
    pub(super) struct Manifest {
        protocol: u32,
        executable: PathBuf,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        configuration: serde_json::Value,
    }

    pub(super) fn manifest(directory: Option<&std::path::Path>, id: &str) -> Option<Manifest> {
        if !cfg!(debug_assertions) {
            return None;
        }
        let directory = directory?;
        let file = std::fs::File::open(directory.join(format!("{id}.json"))).ok()?;
        let entry: Manifest = serde_json::from_reader(file.take(64 * 1024)).ok()?;
        (entry.protocol == wire::VERSION
            && entry.executable.is_absolute()
            && entry.executable.is_file())
        .then_some(entry)
    }

    pub(crate) fn open_in(
        directory: Option<&std::path::Path>,
        spec: &ProviderSpec,
        key: Option<&str>,
    ) -> Result<Box<dyn SampleProvider>, SampleError> {
        let entry = manifest(directory, spec.id)
            .ok_or_else(|| crate::unavailable(spec, super::UNAVAILABLE))?;
        Ok(Box::new(LocalProvider {
            id: spec.id,
            label: spec.label,
            key: key.map(str::to_owned),
            entry,
        }))
    }

    struct LocalProvider {
        id: &'static str,
        label: &'static str,
        key: Option<String>,
        entry: Manifest,
    }

    impl LocalProvider {
        fn call<T: DeserializeOwned>(
            &self,
            operation: Operation,
            fetch: &dyn SampleFetch,
        ) -> Result<T, SampleError> {
            let network = fetch.plugin_network();
            let deadline =
                Instant::now() + Duration::from_secs(network.timeout_seconds.clamp(1, 180) + 2);
            let request = serde_json::to_vec(&Request {
                protocol: wire::VERSION,
                credential: self.key.clone(),
                configuration: self.entry.configuration.clone(),
                network,
                operation,
            })
            .map_err(|_| self.error("could not encode the request"))?;
            let mut child = Command::new(&self.entry.executable)
                .args(&self.entry.args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|_| self.error("could not start the local executable"))?;
            let mut stdin = child.stdin.take().expect("piped stdin");
            let stdout = child.stdout.take().expect("piped stdout");
            let result = std::thread::scope(|scope| {
                let writer = scope.spawn(move || stdin.write_all(&request));
                let reader = scope.spawn(move || {
                    let mut bytes = Vec::new();
                    stdout
                        .take(MAX_JSON_BYTES as u64 + 1)
                        .read_to_end(&mut bytes)?;
                    Ok::<_, std::io::Error>(bytes)
                });
                let exit = loop {
                    match child.try_wait() {
                        Ok(Some(status)) => break Ok(status),
                        Ok(None) if Instant::now() < deadline => {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        _ => {
                            let _ = child.kill();
                            let _ = child.wait();
                            break Err(
                                self.error("local executable timed out or could not be monitored")
                            );
                        }
                    }
                };
                let written = writer.join().ok().and_then(Result::ok);
                let bytes = reader.join().ok().and_then(Result::ok);
                if !exit?.success() || written.is_none() {
                    return Err(self.error("local executable failed"));
                }
                bytes
                    .filter(|bytes| bytes.len() <= MAX_JSON_BYTES)
                    .ok_or_else(|| self.error("invalid response size"))
            })?;
            match serde_json::from_slice::<Reply<T>>(&result) {
                Ok(Reply::Ok { value }) => Ok(value),
                Ok(Reply::Error { code, message }) => match code.as_deref() {
                    Some("unknown_id") => Err(SampleError::refused("unknown_id", message)),
                    Some("no_download") => Err(SampleError::refused("no_download", message)),
                    _ => Err(self.error(&message)),
                },
                Err(_) => Err(self.error("invalid response format")),
            }
        }

        fn error(&self, message: &str) -> SampleError {
            SampleError::Failed(format!("{}: {message}", self.label))
        }
    }

    impl SampleProvider for LocalProvider {
        fn id(&self) -> &'static str {
            self.id
        }
        fn label(&self) -> &'static str {
            self.label
        }

        fn search(
            &self,
            query: &SampleQuery,
            fetch: &dyn SampleFetch,
        ) -> Result<SamplePage, SampleError> {
            let page: SamplePage = self.call(
                Operation::Search {
                    query: query.clone(),
                },
                fetch,
            )?;
            if page.provider != self.id || page.items.iter().any(|item| item.provider != self.id) {
                return Err(self.error("response used a different provider id"));
            }
            Ok(page)
        }

        fn resolve(&self, id: &str, fetch: &dyn SampleFetch) -> Result<SampleItem, SampleError> {
            let item: SampleItem = self.call(Operation::Resolve { id: id.into() }, fetch)?;
            if item.provider != self.id || item.id != id {
                return Err(self.error("response did not match the requested sample"));
            }
            Ok(item)
        }
    }
}

pub(crate) use host::open_in;

/// Entry point for the optional executable containing the documented adapters.
#[cfg(feature = "local-provider-runner")]
pub fn run_adapter() -> Result<(), String> {
    use crate::http::{HttpFetch, ProxyChoice};
    use crate::provider::MAX_JSON_BYTES;
    use std::io::Read;
    use std::time::Duration;
    use wire::{Operation, Reply, Request};

    let id = std::env::args().nth(1).ok_or("missing provider id")?;
    let spec = crate::registry::spec(&id).ok_or("unknown provider")?;
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(MAX_JSON_BYTES as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "could not read the request")?;
    let request: Request = serde_json::from_slice(&bytes).map_err(|_| "invalid request")?;
    if request.protocol != wire::VERSION {
        return Err("unsupported sample plugin protocol".into());
    }
    let provider = crate::builtin_provider(spec, request.credential.as_deref())
        .map_err(|error| error.to_string())?;
    let proxy = match &request.network.proxy {
        Proxy::System => ProxyChoice::System,
        Proxy::None => ProxyChoice::None,
        Proxy::Manual(url) => ProxyChoice::Manual(url),
    };
    let configured_timeout = request
        .configuration
        .get("timeout_seconds")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(request.network.timeout_seconds);
    let timeout = Duration::from_secs(
        configured_timeout
            .min(request.network.timeout_seconds)
            .clamp(1, 180),
    );
    let fetch = HttpFetch::with_proxy(timeout, proxy)?;
    fn write_reply<T: Serialize>(
        result: Result<T, crate::model::SampleError>,
    ) -> Result<(), String> {
        let reply = match result {
            Ok(value) => Reply::Ok { value },
            Err(error) => Reply::Error {
                code: error.code().map(str::to_owned),
                message: error.to_string(),
            },
        };
        serde_json::to_writer(std::io::stdout().lock(), &reply)
            .map_err(|_| "could not write response".into())
    }
    match request.operation {
        Operation::Search { query } => write_reply(provider.search(&query, &fetch)),
        Operation::Resolve { id } => write_reply(provider.resolve(&id, &fetch)),
    }
}
