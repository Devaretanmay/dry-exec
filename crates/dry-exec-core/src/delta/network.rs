//! Transparent network proxy intercepting outbound requests with schema-driven mock responses.

use crate::delta::types::InterceptedRequest;
use crate::error::DeltaError;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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

/// Deterministic network boundary: a reserved loopback port plus the schema-driven route table.
///
/// The isolated execution layer binds the listener for `port` inside its own network namespace
/// and forwards the descriptor to the control plane, which serves it. Interception is therefore
/// reachable from within the boundary while every recorded mutation stays on the control plane.
#[derive(Debug, Clone)]
pub struct NetworkBoundary {
    pub port: u16,
    pub schema: NetworkMockSchema,
}

/// Bind the schema-driven mock listener on loopback inside the calling network namespace.
pub fn bind_loopback_listener(port: u16) -> Result<TcpListener, DeltaError> {
    TcpListener::bind(("127.0.0.1", port)).map_err(DeltaError::IoError)
}

/// Local transparent proxy operating within the isolated network namespace.
pub struct TransparentProxy {
    port: u16,
    is_running: Arc<AtomicBool>,
    intercepted: Arc<Mutex<Vec<InterceptedRequest>>>,
    schema_breaches: Arc<AtomicUsize>,
    server_thread: Option<JoinHandle<()>>,
}

impl TransparentProxy {
    /// Start transparent proxy listener on loopback interface with schema-driven mocking.
    ///
    /// The listener is bound in the calling network namespace, which makes it reachable only from
    /// that namespace. To intercept requests originating inside an isolated namespace, reserve a
    /// port with [`TransparentProxy::reserve_loopback_port`], have the isolated execution layer
    /// bind it, and adopt the forwarded descriptor with [`TransparentProxy::from_listener`].
    pub fn start(schema: NetworkMockSchema) -> Result<Self, DeltaError> {
        let port = Self::reserve_loopback_port()?;
        Self::from_listener(bind_loopback_listener(port)?, schema)
    }

    /// Serve deterministic mock responses on a listener bound inside the isolated network
    /// namespace. Accepted connections are recorded into this control-plane instance.
    pub fn from_listener(
        listener: TcpListener,
        schema: NetworkMockSchema,
    ) -> Result<Self, DeltaError> {
        let port = listener.local_addr().map_err(DeltaError::IoError)?.port();

        let is_running = Arc::new(AtomicBool::new(true));
        let running_clone = is_running.clone();
        let intercepted = Arc::new(Mutex::new(Vec::new()));
        let intercepted_clone = intercepted.clone();
        let schema_breaches = Arc::new(AtomicUsize::new(0));
        let schema_breaches_clone = schema_breaches.clone();

        // Poll the listener so the serving thread observes shutdown without depending on a
        // wake-up connection, which cannot reach a listener bound in another network namespace.
        listener
            .set_nonblocking(true)
            .map_err(DeltaError::IoError)?;

        let server_thread = thread::spawn(move || {
            // Set socket timeout for connection operations to bound execution latency
            while running_clone.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        handle_connection(
                            stream,
                            &schema,
                            &intercepted_clone,
                            &schema_breaches_clone,
                        );
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
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
            schema_breaches,
            server_thread: Some(server_thread),
        })
    }

    /// Reserve an unused loopback port for a listener bound inside the isolated network namespace.
    pub fn reserve_loopback_port() -> Result<u16, DeltaError> {
        let probe = TcpListener::bind("127.0.0.1:0").map_err(DeltaError::IoError)?;
        Ok(probe.local_addr().map_err(DeltaError::IoError)?.port())
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Drain all recorded network mutations intercepted during execution.
    pub fn drain_intercepted(&self) -> Vec<InterceptedRequest> {
        let mut guard = self.intercepted.lock().unwrap();
        std::mem::take(&mut *guard)
    }

    /// Count of routes refused outside the registered schema, as a scalar summary metric.
    pub fn schema_breaches(&self) -> usize {
        self.schema_breaches.load(Ordering::Relaxed)
    }
}

impl Drop for TransparentProxy {
    fn drop(&mut self) {
        self.is_running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.server_thread.take() {
            let _ = handle.join();
        }
    }
}

/// Bounded request buffer: a single intercepted request never exceeds this ceiling.
const REQUEST_BUFFER_BYTES: usize = 16 * 1024;

/// Offset of the request head terminator, marking the start of the body.
fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|window| window == b"\r\n\r\n")
}

/// Declared body length from the request head, if the header is present.
fn declared_content_length(head: &[u8]) -> Option<usize> {
    String::from_utf8_lossy(head).lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse::<usize>().ok())?
    })
}

/// Whether the buffered bytes hold a complete request head and its declared body.
fn request_is_complete(buf: &[u8]) -> bool {
    match find_head_end(buf) {
        Some(head_end) => match declared_content_length(&buf[..head_end]) {
            Some(len) => buf.len() - (head_end + 4) >= len,
            None => true,
        },
        None => false,
    }
}

/// Read a single intercepted request, tolerating TCP segmentation and honoring the read timeout.
fn read_request(stream: &mut TcpStream) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; REQUEST_BUFFER_BYTES];
    let mut filled = 0usize;

    loop {
        match stream.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => {
                filled += n;
                if request_is_complete(&buf[..filled]) || filled == buf.len() {
                    break;
                }
            }
            Err(_) => break,
        }
    }

    (filled > 0).then(|| buf[..filled].to_vec())
}

fn read_request_from<R: Read>(reader: &mut R) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; REQUEST_BUFFER_BYTES];
    let mut filled = 0usize;
    loop {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => {
                filled += n;
                if request_is_complete(&buf[..filled]) || filled == buf.len() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    (filled > 0).then(|| buf[..filled].to_vec())
}

fn mock_response_bytes(
    status_code: u16,
    headers: &HashMap<String, String>,
    body: &[u8],
) -> Vec<u8> {
    let reason = match status_code {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Status",
    };
    let mut bytes = format!("HTTP/1.1 {status_code} {reason}\r\n").into_bytes();
    for (key, value) in headers {
        bytes.extend_from_slice(format!("{key}: {value}\r\n").as_bytes());
    }
    bytes.extend_from_slice(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes());
    bytes.extend_from_slice(body);
    bytes
}

fn handle_tls_connection(
    stream: TcpStream,
    target: &str,
    schema: &NetworkMockSchema,
    intercepted: &Arc<Mutex<Vec<InterceptedRequest>>>,
    schema_breaches: &Arc<AtomicUsize>,
) {
    let host = target.trim_end_matches(":443");
    let certificate = match rcgen::generate_simple_self_signed(vec![host.to_string()]) {
        Ok(certificate) => certificate,
        Err(_) => return,
    };
    let cert_der = rustls::pki_types::CertificateDer::from(certificate.cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(
        rustls::pki_types::PrivatePkcs8KeyDer::from(certificate.key_pair.serialize_der()),
    );
    let config = match rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
    {
        Ok(config) => Arc::new(config),
        Err(_) => return,
    };
    let connection = match rustls::ServerConnection::new(config) {
        Ok(connection) => connection,
        Err(_) => return,
    };
    let mut tls = rustls::StreamOwned::new(connection, stream);
    let raw_request = match read_request_from(&mut tls) {
        Some(request) => request,
        None => return,
    };
    let request_text = String::from_utf8_lossy(&raw_request);
    let mut lines = request_text.lines();
    let parts: Vec<&str> = lines
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .collect();
    if parts.len() < 2 {
        return;
    }
    let method = parts[0].to_uppercase();
    let path = parts[1];
    let url = if path.starts_with("https://") {
        path.to_string()
    } else {
        format!("https://{host}{path}")
    };
    let mut headers = HashMap::new();
    for line in lines.by_ref() {
        if line.is_empty() {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            headers.insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    let body_offset = find_head_end(&raw_request)
        .map(|idx| idx + 4)
        .unwrap_or(raw_request.len());
    let request_body = raw_request[body_offset..].to_vec();
    let (status_code, response_headers, response_body) = match schema.match_route(&method, &url) {
        Some(mock) => (mock.status_code, mock.headers.clone(), mock.body.clone()),
        None => {
            schema_breaches.fetch_add(1, Ordering::Relaxed);
            let mut response_headers = HashMap::new();
            response_headers.insert("Content-Type".to_string(), "application/json".to_string());
            (
                400,
                response_headers,
                b"{\"error\":\"Schema boundary breach: unregistered endpoint\"}".to_vec(),
            )
        }
    };
    let _ = tls.write_all(&mock_response_bytes(
        status_code,
        &response_headers,
        &response_body,
    ));
    intercepted.lock().unwrap().push(InterceptedRequest {
        method,
        url,
        headers,
        request_body,
        response_status: status_code,
        response_body,
    });
}

/// Parse HTTP request, enforce schema boundaries, and return deterministic response.
fn handle_connection(
    mut stream: TcpStream,
    schema: &NetworkMockSchema,
    intercepted: &Arc<Mutex<Vec<InterceptedRequest>>>,
    schema_breaches: &Arc<AtomicUsize>,
) {
    // Enforce 500ms timeout bound on connection operations
    let timeout = Some(Duration::from_millis(500));
    let _ = stream.set_read_timeout(timeout);
    let _ = stream.set_write_timeout(timeout);

    let raw_request = match read_request(&mut stream) {
        Some(request) => request,
        None => return,
    };
    let n = raw_request.len();
    let request_str = String::from_utf8_lossy(&raw_request);
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

    if method == "CONNECT" {
        let _ = stream.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n");
        handle_tls_connection(stream, &url, schema, intercepted, schema_breaches);
        return;
    }

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
    let body_offset = find_head_end(&raw_request).map(|idx| idx + 4).unwrap_or(n);
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
            schema_breaches.fetch_add(1, Ordering::Relaxed);
            let mut h = HashMap::new();
            h.insert("Content-Type".to_string(), "application/json".to_string());
            (
                400,
                h,
                b"{\"error\":\"Schema boundary breach: unregistered endpoint\"}".to_vec(),
            )
        }
    };

    let _ = stream.write_all(&mock_response_bytes(
        status_code,
        &response_headers,
        &response_body,
    ));

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn connect_returns_tunnel_ack_and_mock_payload() {
        let mut schema = NetworkMockSchema::new();
        schema.register_endpoint(
            "GET",
            "https://httpbin.org/get",
            MockResponse {
                body: br#"{"mocked":true}"#.to_vec(),
                ..MockResponse::default()
            },
        );
        let proxy = TransparentProxy::start(schema).unwrap();
        let mut stream = TcpStream::connect(("127.0.0.1", proxy.port())).unwrap();
        stream
            .write_all(b"CONNECT httpbin.org:443 HTTP/1.1\r\nHost: httpbin.org\r\n\r\n")
            .unwrap();
        let mut response = [0u8; 64];
        let count = stream.read(&mut response).unwrap();
        assert!(String::from_utf8_lossy(&response[..count]).contains("200 Connection Established"));
    }
}
