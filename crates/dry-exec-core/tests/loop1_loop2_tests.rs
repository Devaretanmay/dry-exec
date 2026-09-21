//! Containerized verification suite for Loop 1 and Loop 2 primitives.
#![cfg(target_os = "linux")]

use dry_exec_core::delta::{
    scan_dirty_pages, AnonymousMemoryRegion, DeltaCoordinator, MockResponse, NetworkBoundary,
    NetworkMockSchema, TransparentProxy,
};
use dry_exec_core::error::BoundaryExitStatus;
use dry_exec_core::isolation::{
    execute_isolated_process, execute_isolated_process_with_inspection, ProcessBoundaryConfig,
    SeccompFilter, SyscallAction,
};
use std::time::Instant;

/// Boundary configuration with deterministic SECCOMP_RET_TRAP interception.
fn trap_boundary_config() -> ProcessBoundaryConfig {
    ProcessBoundaryConfig {
        seccomp_filter: SeccompFilter::new(SyscallAction::Trap).with_baseline_whitelist(),
        ..Default::default()
    }
}

/// Boundary configuration permitting outbound request primitives so the isolated execution layer
/// can reach the transparent proxy inside its own network namespace.
fn network_boundary_config(boundary: NetworkBoundary) -> ProcessBoundaryConfig {
    let filter = SeccompFilter::new(SyscallAction::Trap)
        .with_baseline_whitelist()
        .allow(libc::SYS_socket)
        .allow(libc::SYS_connect)
        .allow(libc::SYS_sendto)
        .allow(libc::SYS_recvfrom);

    ProcessBoundaryConfig {
        seccomp_filter: filter,
        network_boundary: Some(boundary),
        ..Default::default()
    }
}

#[test]
fn test_assertion_a_syscall_isolation_interception() {
    // Assertion A: Attempting a blocked syscall (e.g., socket) must result in
    // BoundaryExitStatus::SyscallViolation containing exact syscall_nr, with zero impact on parent.
    let config = trap_boundary_config();

    let (status, _) = execute_isolated_process(&config, || {
        // Attempt blocked socket syscall: AF_INET, SOCK_STREAM, 0
        unsafe {
            let _ = libc::syscall(libc::SYS_socket, libc::AF_INET, libc::SOCK_STREAM, 0);
        }
    })
    .expect("Process boundary execution failed");

    match status {
        BoundaryExitStatus::SyscallViolation { syscall_nr, .. } => {
            assert_eq!(
                syscall_nr as i64,
                libc::SYS_socket,
                "Expected intercepted syscall to be SYS_socket ({}), observed {}",
                libc::SYS_socket,
                syscall_nr
            );
        }
        other => panic!(
            "Expected BoundaryExitStatus::SyscallViolation, observed {:?}",
            other
        ),
    }
}

#[test]
fn test_assertion_b_state_delta_precision() {
    // Assertion B: Map a 10MB anonymous memory region. Instruct the isolated child to mutate
    // exactly 8 bytes at offset 4096 (Page 1) and 4 bytes at offset 12288 (Page 3).
    // The delta engine must return exactly two PageMutation entries matching exact offsets and values.
    let region_size = 10 * 1024 * 1024; // 10MB
    let mut region = AnonymousMemoryRegion::allocate(region_size).expect("Mmap allocation failed");

    // Pre-populate baseline state in the control plane mapping (CoW source)
    let baseline_slice = region.as_mut_slice();
    for (i, byte) in baseline_slice.iter_mut().enumerate() {
        *byte = (i % 251) as u8;
    }

    let mut coordinator = DeltaCoordinator::new();
    let temp_dir = tempfile::tempdir().expect("Failed to create temporary directory");
    coordinator
        .snapshot_baseline(Some(temp_dir.path()))
        .expect("Baseline snapshot failed");

    let raw_ptr = region.as_ptr() as usize;
    // Hand the isolated action a path only: moving the `TempDir` guard into the child would
    // unlink the ephemeral directory from inside the boundary when the closure is dropped.
    let ephemeral_root = temp_dir.path().to_path_buf();
    let config = trap_boundary_config();

    let (exit_status, inspected) = execute_isolated_process_with_inspection(
        &config,
        move || {
            // Mutate Page 1 (offset 4096): 8 bytes
            let page1_target = (raw_ptr + 4096) as *mut u8;
            let page1_mutation = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x11, 0x22];
            unsafe {
                std::ptr::copy_nonoverlapping(
                    page1_mutation.as_ptr(),
                    page1_target,
                    page1_mutation.len(),
                );
            }

            // Mutate Page 3 (offset 12288): 4 bytes
            let page3_target = (raw_ptr + 12288) as *mut u8;
            let page3_mutation = [0xDE, 0xAD, 0xBE, 0xEF];
            unsafe {
                std::ptr::copy_nonoverlapping(
                    page3_mutation.as_ptr(),
                    page3_target,
                    page3_mutation.len(),
                );
            }

            // Ephemeral filesystem mutation: create one file in the tmpfs overlay
            let file_path = ephemeral_root.join("mutated_state.bin");
            std::fs::write(file_path, b"ephemeral_state_delta")
                .expect("Failed to write ephemeral file");
        },
        // Inspect the live isolated execution layer while its CoW address space is retained
        |context| coordinator.compute_full_delta(context.child_pid, Some((&region, region_size))),
    )
    .expect("Process boundary execution failed");

    assert_eq!(exit_status, BoundaryExitStatus::Exited(0));

    let delta = inspected
        .expect("Inspection closure did not execute against the live boundary")
        .expect("Delta computation failed");

    // Assert exactly 2 mutated pages
    assert_eq!(
        delta.memory_mutations.len(),
        2,
        "Expected exactly 2 mutated pages, found {}",
        delta.memory_mutations.len()
    );

    // Verify Page 1 mutation
    let page1 = &delta.memory_mutations[0];
    assert_eq!(page1.page_index, 1);
    assert_eq!(page1.deltas.len(), 1);
    assert_eq!(page1.deltas[0].offset, 4096);
    assert_eq!(
        page1.deltas[0].mutated,
        vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x11, 0x22]
    );

    // Verify Page 3 mutation
    let page3 = &delta.memory_mutations[1];
    assert_eq!(page3.page_index, 3);
    assert_eq!(page3.deltas.len(), 1);
    assert_eq!(page3.deltas[0].offset, 12288);
    assert_eq!(page3.deltas[0].mutated, vec![0xDE, 0xAD, 0xBE, 0xEF]);

    // Verify filesystem mutation
    assert_eq!(delta.fs_mutations.len(), 1);
}

#[test]
fn test_assertion_c_computational_complexity_bound() {
    // Assertion C: The time taken to compute the delta for the 10MB region with 2 mutated
    // pages must be strictly bounded under 1 millisecond, proving O(P_dirty) complexity.
    let region_size = 10 * 1024 * 1024; // 10MB
    let mut region = AnonymousMemoryRegion::allocate(region_size).expect("Mmap allocation failed");

    let pid = std::process::id() as i32;

    // Fault in every page before baseline reset so /proc/[pid]/clear_refs can write-protect
    // the PTEs and arm soft-dirty tracking for subsequent writes.
    for byte in region.as_mut_slice().iter_mut() {
        *byte = 0;
    }

    // Retain the pre-execution baseline bytes: the control plane mapping is the comparison source.
    let baseline = region.as_slice().to_vec();
    let base_addr = region.as_ptr() as usize;

    // Reset baseline tracking
    dry_exec_core::delta::clear_soft_dirty_bits(pid).expect("Failed to clear soft-dirty bits");

    // Mutate 2 pages
    {
        let slice = region.as_mut_slice();
        slice[4096] = 0xFF;
        slice[12288] = 0xAA;
    }

    let start = Instant::now();
    let mutations =
        scan_dirty_pages(pid, base_addr, region_size, &baseline).expect("Dirty page scan failed");
    let elapsed = start.elapsed();

    assert_eq!(mutations.len(), 2);
    assert!(
        elapsed.as_micros() < 1000,
        "Delta scan duration {} us exceeded 1000 us (1ms) bound",
        elapsed.as_micros()
    );
}

#[test]
fn test_assertion_e_network_boundary_interception() {
    // Assertion E: The isolated execution layer occupies its own CLONE_NEWNET, so the
    // transparent proxy must be reachable from within that namespace. The boundary binds the
    // schema-driven mock listener there and forwards the descriptor to the control plane, which
    // serves it: the outbound request is intercepted and recorded as a network mutation.
    let mut schema = NetworkMockSchema::new();
    schema.register_endpoint(
        "POST",
        "/v1/charges",
        MockResponse {
            status_code: 200,
            headers: std::collections::HashMap::from([(
                "Content-Type".to_string(),
                "application/json".to_string(),
            )]),
            body: b"{\"id\":\"ch_mock_9988\"}".to_vec(),
        },
    );

    let port = TransparentProxy::reserve_loopback_port().expect("Failed to reserve proxy port");
    let config = network_boundary_config(NetworkBoundary { port, schema });

    let mut coordinator = DeltaCoordinator::new();
    let request = b"POST /v1/charges HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 2\r\n\r\n{}";

    let (exit_status, inspected) = execute_isolated_process_with_inspection(
        &config,
        move || {
            // Connect to the mock listener bound inside this network namespace. Distinct exit
            // codes localize a handshake failure without crossing the boundary with diagnostics.
            let sock = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
            if sock < 0 {
                unsafe { libc::_exit(91) }
            }

            let mut address: libc::sockaddr_in = unsafe { std::mem::zeroed() };
            address.sin_family = libc::AF_INET as libc::sa_family_t;
            address.sin_port = port.to_be();
            address.sin_addr.s_addr = u32::from(std::net::Ipv4Addr::LOCALHOST).to_be();

            let connected = unsafe {
                libc::connect(
                    sock,
                    &address as *const libc::sockaddr_in as *const libc::sockaddr,
                    std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
                )
            };
            if connected != 0 {
                unsafe { libc::_exit(92) }
            }

            let sent = unsafe {
                libc::send(
                    sock,
                    request.as_ptr() as *const libc::c_void,
                    request.len(),
                    libc::MSG_NOSIGNAL,
                )
            };
            if sent != request.len() as isize {
                unsafe { libc::_exit(93) }
            }

            let mut response = [0u8; 1024];
            let mut received = 0usize;
            while received < response.len() {
                let n = unsafe {
                    libc::recv(
                        sock,
                        response[received..].as_mut_ptr() as *mut libc::c_void,
                        response.len() - received,
                        0,
                    )
                };
                if n <= 0 {
                    break;
                }
                received += n as usize;
            }

            if received == 0 {
                unsafe { libc::_exit(94) }
            }

            let _ = unsafe { libc::close(sock) };
        },
        |context| {
            if let Some(proxy) = context.network {
                coordinator.set_network_proxy(proxy);
            }
            coordinator.compute_full_delta(context.child_pid, None)
        },
    )
    .expect("Process boundary execution failed");

    assert_eq!(
        exit_status,
        BoundaryExitStatus::Exited(0),
        "Isolated execution layer failed to complete the network boundary handshake"
    );

    let delta = inspected
        .expect("Inspection closure did not execute against the live boundary")
        .expect("Delta computation failed");

    assert_eq!(
        delta.network_mutations.len(),
        1,
        "Expected exactly 1 intercepted network mutation, found {}",
        delta.network_mutations.len()
    );

    let mutation = &delta.network_mutations[0];
    assert_eq!(mutation.method, "POST");
    assert_eq!(mutation.url, "/v1/charges");
    assert_eq!(mutation.request_body, b"{}".to_vec());
    assert_eq!(mutation.response_status, 200);
    assert_eq!(
        mutation.response_body,
        b"{\"id\":\"ch_mock_9988\"}".to_vec()
    );
}
