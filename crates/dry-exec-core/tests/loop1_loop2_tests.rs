//! Containerized verification suite for Loop 1 and Loop 2 primitives.

use std::time::Instant;
use dry_exec_core::delta::{scan_dirty_pages, AnonymousMemoryRegion, DeltaCoordinator};
use dry_exec_core::error::BoundaryExitStatus;
use dry_exec_core::isolation::{
    execute_isolated_process, ProcessBoundaryConfig, SeccompFilter, SyscallAction,
};

#[test]
fn test_assertion_a_syscall_isolation_interception() {
    // Assertion A: Attempting a blocked syscall (e.g., socket) must result in
    // BoundaryExitStatus::SyscallViolation containing exact syscall_nr, with zero impact on parent.
    let mut config = ProcessBoundaryConfig::default();
    config.seccomp_filter = SeccompFilter::new(SyscallAction::Trap).with_baseline_whitelist();

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
        other => panic!("Expected BoundaryExitStatus::SyscallViolation, observed {:?}", other),
    }
}

#[test]
fn test_assertion_b_state_delta_precision() {
    // Assertion B: Map a 10MB anonymous memory region. Instruct the isolated child to mutate
    // exactly 8 bytes at offset 4096 (Page 1) and 4 bytes at offset 12288 (Page 3).
    // The delta engine must return exactly two PageMutation entries matching exact offsets and values.
    let region_size = 10 * 1024 * 1024; // 10MB
    let mut region = AnonymousMemoryRegion::allocate(region_size).expect("Mmap allocation failed");

    // Pre-populate baseline state
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

    let mut config = ProcessBoundaryConfig::default();
    config.seccomp_filter = SeccompFilter::new(SyscallAction::Trap).with_baseline_whitelist();

    let (exit_status, _) = execute_isolated_process(&config, move || {
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

        // Ephemeral filesystem mutation: create one file in tmpfs overlay
        let file_path = temp_dir.path().join("mutated_state.bin");
        std::fs::write(file_path, b"ephemeral_state_delta").expect("Failed to write ephemeral file");
    })
    .expect("Process boundary execution failed");

    assert_eq!(exit_status, BoundaryExitStatus::Exited(0));

    let delta = coordinator
        .compute_full_delta(std::process::id() as i32, Some((&region, region_size)))
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

    // Reset baseline tracking
    dry_exec_core::delta::clear_soft_dirty_bits(pid).expect("Failed to clear soft-dirty bits");

    // Mutate 2 pages
    let slice = region.as_mut_slice();
    slice[4096] = 0xFF;
    slice[12288] = 0xAA;

    let base_addr = region.as_ptr() as usize;

    let start = Instant::now();
    let mutations = scan_dirty_pages(pid, base_addr, region_size, region.as_slice())
        .expect("Dirty page scan failed");
    let elapsed = start.elapsed();

    assert_eq!(mutations.len(), 2);
    assert!(
        elapsed.as_micros() < 1000,
        "Delta scan duration {} us exceeded 1000 us (1ms) bound",
        elapsed.as_micros()
    );
}
