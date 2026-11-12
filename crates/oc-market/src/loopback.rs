//! A throwaway HTTP/1.1 server for rehearsing the install path on loopback.
//!
//! Tests serve a fixture catalog and a fixture archive from here, so a download
//! is a real HTTP download without touching the public internet.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// What one route serves.
#[derive(Clone)]
enum Route {
    /// A body that is already whole in memory.
    Bytes(Vec<u8>),
    /// `chunk` repeated up to `total` bytes, sent with chunked encoding so the
    /// response carries no `Content-Length`.
    Stream { chunk: Vec<u8>, total: usize },
}

/// A loopback server that answers `GET` from an in-memory route table.
pub struct FixtureServer {
    addr: SocketAddr,
    routes: Arc<Mutex<HashMap<String, Route>>>,
    requests: Arc<Mutex<Vec<String>>>,
    written: Arc<Mutex<HashMap<String, usize>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl FixtureServer {
    /// Bind an ephemeral loopback port and serve in a background thread.
    pub fn start() -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let addr = listener.local_addr().expect("loopback address");
        let routes = Arc::new(Mutex::new(HashMap::new()));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let written = Arc::new(Mutex::new(HashMap::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let routes = routes.clone();
            let requests = requests.clone();
            let written = written.clone();
            let stop = stop.clone();
            thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            // An accepted socket inherits O_NONBLOCK on macOS.
                            if stream.set_nonblocking(false).is_ok() {
                                serve(stream, &routes, &requests, &written);
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2));
                        }
                        Err(_) => thread::sleep(Duration::from_millis(1)),
                    }
                }
            })
        };
        Self {
            addr,
            routes,
            requests,
            written,
            stop,
            thread: Some(thread),
        }
    }

    /// Absolute URL for one route on this server.
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, ensure_slash(path))
    }

    /// Absolute URL for the server root.
    pub fn base(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Serve `body` at `path` from now on.
    pub fn publish(&self, path: &str, body: Vec<u8>) {
        self.routes
            .lock()
            .unwrap()
            .insert(ensure_slash(path), Route::Bytes(body));
    }

    /// Serve UTF-8 text (a catalog or an API response) at `path`.
    pub fn publish_text(&self, path: &str, body: impl Into<String>) {
        self.publish(path, body.into().into_bytes());
    }

    /// Serve `chunk` repeated up to `total` bytes at `path`, with chunked
    /// encoding and no `Content-Length`, so a client cannot learn the size
    /// before it has read the body.
    pub fn stream(&self, path: &str, chunk: Vec<u8>, total: usize) {
        assert!(!chunk.is_empty(), "a streamed body needs a chunk");
        self.routes
            .lock()
            .unwrap()
            .insert(ensure_slash(path), Route::Stream { chunk, total });
    }

    /// Body bytes sent at `path` so far. A client that enforces its cap while
    /// reading leaves this well below the published total.
    pub fn written(&self, path: &str) -> usize {
        self.written
            .lock()
            .unwrap()
            .get(&ensure_slash(path))
            .copied()
            .unwrap_or(0)
    }

    /// Stop serving one route; requests for it answer 404.
    pub fn remove(&self, path: &str) {
        self.routes.lock().unwrap().remove(&ensure_slash(path));
    }

    /// Paths requested since the last call, in order.
    pub fn requests(&self) -> Vec<String> {
        std::mem::take(&mut *self.requests.lock().unwrap())
    }
}

impl Drop for FixtureServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn ensure_slash(path: &str) -> String {
    if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    }
}

fn serve(
    mut stream: TcpStream,
    routes: &Mutex<HashMap<String, Route>>,
    requests: &Mutex<Vec<String>>,
    written: &Mutex<HashMap<String, usize>>,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let Ok(reader_stream) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(reader_stream);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
        return;
    }
    // Drain the headers so the client's write completes.
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) if line == "\r\n" || line == "\n" => break,
            Ok(_) => {}
        }
    }
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .split(['?', '#'])
        .next()
        .unwrap_or("/")
        .to_string();
    requests.lock().unwrap().push(path.clone());
    let route = routes.lock().unwrap().get(&path).cloned();
    match route {
        Some(Route::Bytes(payload)) => {
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                payload.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.write_all(&payload);
            let _ = stream.flush();
            record_written(written, &path, payload.len());
        }
        Some(Route::Stream { chunk, total }) => {
            let mut writer = BufWriter::new(&mut stream);
            let head = "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
            if writer.write_all(head.as_bytes()).is_err() {
                return;
            }
            let mut sent = 0;
            while sent < total {
                let size = chunk.len().min(total - sent);
                if write!(writer, "{size:x}\r\n").is_err()
                    || writer.write_all(&chunk[..size]).is_err()
                    || writer.write_all(b"\r\n").is_err()
                {
                    break;
                }
                sent += size;
                record_written(written, &path, size);
            }
            // The final chunk closes the body, even when the client hung up
            // early and the write above failed.
            let _ = writer.write_all(b"0\r\n\r\n");
            let _ = writer.flush();
        }
        None => {
            let response = "HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    }
}

fn record_written(written: &Mutex<HashMap<String, usize>>, path: &str, bytes: usize) {
    *written.lock().unwrap().entry(path.to_string()).or_default() += bytes;
}
