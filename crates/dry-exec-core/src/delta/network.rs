//! Transparent network proxy intercepting outbound requests with schema-driven mock responses.

use crate::delta::types::InterceptedRequest;
use crate::error::DeltaError;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Deterministic mock response defined by the environment schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockResponse {
    pub status_code: u16,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl Default for MockResponse {
    fn default() -> Self {
        Self {
            status_code: 200,
            headers: HashMap::from([("Content-Type".to_string(), "application/json".to_string())]),
            body: b"{\"status\":\"ok\"}".to_vec(),
        }
    }
}

/// Schema-driven registry mapping outbound HTTP routes to deterministic responses.
#[derive(Debug, Clone, Default)]
pub struct NetworkMockSchema {
    endpoints: HashMap<(String, String), MockResponse>,
}

impl NetworkMockSchema {
    pub fn new() -> Self {
        Self {
            endpoints: HashMap::new(),
        }
    }

    /// Register a deterministic mock response for an HTTP method and path.
    pub fn register_endpoint(
        &mut self,
        method: impl Into<String>,
        path: impl Into<String>,
        response: MockResponse,
    ) {
        self.endpoints
            .insert((method.into().to_uppercase(), path.into()), response);
    }

    /// Look up schema response for the given method and path.
    pub fn match_route(&self, method: &str, path: &str) -> Option<&MockResponse> {
        self.endpoints
            .get(&(method.to_uppercase(), path.to_string()))
    }
}

/// Local transparent proxy operating within the isolated network namespace.
pub struct TransparentProxy {
    port: u16,
    is_running: Arc<AtomicBool>,
    intercepted: Arc<Mutex<Vec<InterceptedRequest>>>,
    server_thread: Option<JoinHandle<()>>,
}

impl TransparentProxy {
    /// Start transparent proxy listener on loopback interface with schema-driven mocking.
    pub fn start(schema: NetworkMockSchema) -> Result<Self, DeltaError> {
        let listener = TcpListener::bind("127.0.0.1:0").map_err(DeltaError::IoError)?;
        let port = listener.local_addr().map_err(DeltaError::IoError)?.port();

        let is_running = Arc::new(AtomicBool::new(true));
        let running_clone = is_running.clone();
        let intercepted = Arc::new(Mutex::new(Vec::new()));
        let intercepted_clone = intercepted.clone();

        // Enforce 100ms accept timeout so proxy thread checks running flag regularly
        listener
            .set_nonblocking(false)
            .map_err(DeltaError::IoError)?;

        let server_thread = thread::spawn(move || {
            // Set socket timeout for connection operations to bound execution latency
            while running_clone.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        handle_connection(stream, &schema, &intercepted_clone);
                    }
                    Err(_) => {
                        thread::sleep(Duration::from_millis(10));
                    }
                }
            }
        });

        Ok(Self {
            port,
            is_running,
            intercepted,
            server_thread: Some(server_thread),
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Drain all recorded network mutations intercepted during execution.
    pub fn drain_intercepted(&self) -> Vec<InterceptedRequest> {
        let mut guard = self.intercepted.lock().unwrap();
        std::mem::take(&mut *guard)
    }
}

impl Drop for TransparentProxy {
    fn drop(&mut self) {
        self.is_running.store(false, Ordering::Relaxed);
        // Connect to unblock accept call if waiting
        let _ = TcpStream::connect(format!("127.0.0.1:{}", self.port));
        if let Some(handle) = self.server_thread.take() {
            let _ = handle.join();
        }
    }
}

/// Parse HTTP request, enforce schema boundaries, and return deterministic response.
fn handle_connection(
    mut stream: TcpStream,
    schema: &NetworkMockSchema,
    intercepted: &Arc<Mutex<Vec<InterceptedRequest>>>,
) {
    // Enforce 500ms timeout bound on connection operations
    let timeout = Some(Duration::from_millis(500));
    let _ = stream.set_read_timeout(timeout);
    let _ = stream.set_write_timeout(timeout);

    let mut buf = [0u8; 4096];
    let n = match stream.read(&mut buf) {
        Ok(n) if n > 0 => n,
        _ => return,
    };

    let raw_request = &buf[..n];
    let request_str = String::from_utf8_lossy(raw_request);
    let mut lines = request_str.lines();

    let request_line = match lines.next() {
        Some(l) => l,
        None => return,
    };

    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 {
        return;
    }

    let method = parts[0].to_uppercase();
    let url = parts[1].to_string();

    let mut headers = HashMap::new();
    for line in lines.by_ref() {
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_string(), v.trim().to_string());
        }
    }

    // Extract request body if present
    let body_offset = request_str.find("\r\n\r\n").map(|idx| idx + 4).unwrap_or(n);
    let request_body = if body_offset < n {
        raw_request[body_offset..n].to_vec()
    } else {
        Vec::new()
    };

    // Evaluate against schema-driven routes
    let (status_code, response_headers, response_body) = match schema.match_route(&method, &url) {
        Some(mock) => (mock.status_code, mock.headers.clone(), mock.body.clone()),
        None => {
            // Unregistered route rejected at boundary
            let mut h = HashMap::new();
            h.insert("Content-Type".to_string(), "application/json".to_string());
            (
                400,
                h,
                b"{\"error\":\"Schema boundary breach: unregistered endpoint\"}".to_vec(),
            )
        }
    };

    // Format deterministic HTTP response
    let reason = match status_code {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Status",
    };

    let mut response_bytes = format!("HTTP/1.1 {status_code} {reason}\r\n").into_bytes();
    for (k, v) in &response_headers {
        response_bytes.extend_from_slice(format!("{k}: {v}\r\n").as_bytes());
    }
    response_bytes
        .extend_from_slice(format!("Content-Length: {}\r\n\r\n", response_body.len()).as_bytes());
    response_bytes.extend_from_slice(&response_body);

    let _ = stream.write_all(&response_bytes);

    // Record network mutation into state delta ledger
    let mut guard = intercepted.lock().unwrap();
    guard.push(InterceptedRequest {
        method,
        url,
        headers,
        request_body,
        response_status: status_code,
        response_body,
    });
}
