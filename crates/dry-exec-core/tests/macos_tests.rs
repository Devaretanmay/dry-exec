//! macOS Seatbelt and APFS Copy-on-Write integration tests.
#![cfg(target_os = "macos")]

use dry_exec_core::delta::{
    apfs_clone_directory, compute_macos_fs_delta, FsMutation, MockResponse, NetworkMockSchema,
    TransparentProxy,
};
use dry_exec_core::isolation::{generate_seatbelt_profile, SeatbeltConfig};
use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use tempfile::tempdir;

#[test]
fn test_macos_sbpl_generation() {
    let config = SeatbeltConfig::default();
    let profile = generate_seatbelt_profile(&config);

    assert!(profile.contains("(version 1)"));
    assert!(profile.contains("(deny default)"));
    assert!(profile.contains("(allow process-exec (literal \"/bin/echo\"))"));
    assert!(profile.contains("(allow network-outbound (to ip \"localhost:*\"))"));
    assert!(profile.contains("(allow file-write* (subpath \"/private/tmp/dex_ephemeral\"))"));
}

#[test]
fn test_macos_apfs_clone_and_diffing() {
    let base_tmp = tempdir().expect("base tempdir");
    let ephem_tmp = tempdir().expect("ephemeral tempdir");

    let original_file = base_tmp.path().join("config.json");
    fs::write(&original_file, b"{\"env\": \"baseline\"}").expect("write file");

    let clone_dst = ephem_tmp.path().join("ephemeral_workspace");
    apfs_clone_directory(base_tmp.path(), &clone_dst).expect("clone directory");

    assert!(clone_dst.join("config.json").exists());

    // Apply mutations in the ephemeral clone
    fs::write(clone_dst.join("config.json"), b"{\"env\": \"mutated\"}").expect("modify file");
    fs::write(clone_dst.join("output.log"), b"new logs").expect("create file");

    let deltas = compute_macos_fs_delta(base_tmp.path(), &clone_dst).expect("compute deltas");
    assert_eq!(deltas.len(), 2);

    let has_created = deltas
        .iter()
        .any(|m| matches!(m, FsMutation::Created { .. }));
    let has_modified = deltas
        .iter()
        .any(|m| matches!(m, FsMutation::Modified { .. }));
    assert!(has_created);
    assert!(has_modified);
}

#[test]
fn test_macos_network_proxy_interception() {
    let mut schema = NetworkMockSchema::new();
    schema.register_endpoint(
        "GET",
        "/v1/health",
        MockResponse {
            status_code: 200,
            headers: [("Content-Type".into(), "application/json".into())].into(),
            body: b"{\"status\":\"ok\"}".to_vec(),
        },
    );

    let proxy = TransparentProxy::start(schema).expect("start proxy");
    let port = proxy.port();

    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect to proxy");
    let req = b"GET /v1/health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n";
    stream.write_all(req).expect("write request");

    let mut resp = Vec::new();
    stream.read_to_end(&mut resp).expect("read response");
    assert!(resp.starts_with(b"HTTP/1.1 200 OK"));

    let captured = proxy.drain_intercepted();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].method, "GET");
    assert_eq!(captured[0].url, "/v1/health");
}
