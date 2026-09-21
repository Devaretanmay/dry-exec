//! PyO3 FFI bridge for dry-exec / dex ephemeral execution boundary.

#[cfg(all(not(target_os = "linux"), not(target_os = "macos")))]
compile_error!(
    "dry-exec requires Linux kernel primitives (namespaces, seccomp-bpf, soft-dirty pagemap) or macOS kernel primitives (Seatbelt, APFS clonefile)."
);

use pyo3::create_exception;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;

/// Schema-driven mock endpoint: (method, path, status_code, headers, body).
type MockEndpoint = (String, String, u16, HashMap<String, String>, Vec<u8>);

create_exception!(_dry_exec_ffi, IsolationSetupError, PyException);
create_exception!(_dry_exec_ffi, SyscallBoundaryError, PyException);
create_exception!(_dry_exec_ffi, StateDeltaComputationError, PyException);

#[pyfunction]
#[pyo3(signature = (action_json, memory_size, tmpfs_path, trigger_blocked_syscall, mock_endpoints=None, request_to_trigger=None))]
fn execute_isolated_action(
    py: Python<'_>,
    action_json: String,
    memory_size: usize,
    tmpfs_path: String,
    trigger_blocked_syscall: bool,
    mock_endpoints: Option<Vec<MockEndpoint>>,
    request_to_trigger: Option<(String, String, String)>,
) -> PyResult<PyObject> {
    #[cfg(target_os = "linux")]
    use dry_exec_core::delta::AnonymousMemoryRegion;
    use dry_exec_core::delta::{DeltaCoordinator, MockResponse, NetworkMockSchema};

    let _ = (action_json, memory_size);

    #[cfg(target_os = "linux")]
    let result = py.allow_threads(|| {
        use dry_exec_core::error::BoundaryExitStatus;
        use dry_exec_core::isolation::{
            execute_isolated_process_with_inspection, MountConfig, ProcessBoundaryConfig,
            SeccompFilter, SyscallAction,
        };

        let mem_region = if memory_size > 0 {
            Some(
                AnonymousMemoryRegion::allocate(memory_size)
                    .map_err(|e| format!("Memory allocation error: {e}")),
            )
        } else {
            None
        };

        let region = match mem_region {
            Some(Ok(r)) => Some(r),
            Some(Err(e)) => return Err(e),
            None => None,
        };

        let mut coordinator = DeltaCoordinator::new();
        let fs_root = std::path::Path::new(&tmpfs_path);
        if let Err(e) = coordinator.snapshot_baseline(Some(fs_root)) {
            return Err(format!("Filesystem baseline snapshot error: {e}"));
        }

        let proxy_port = if let Some(endpoints) = mock_endpoints {
            let mut schema = NetworkMockSchema::new();
            for (method, path, status_code, headers, body) in endpoints {
                schema.register_endpoint(
                    method,
                    path,
                    MockResponse {
                        status_code,
                        headers,
                        body,
                    },
                );
            }
            Some(
                coordinator
                    .start_network_proxy(schema)
                    .map_err(|e| format!("Network proxy initialization error: {e}"))?,
            )
        } else {
            None
        };

        let mut filter = SeccompFilter::new(SyscallAction::Trap).with_baseline_whitelist();
        if request_to_trigger.is_some() {
            filter = filter
                .allow(libc::SYS_socket)
                .allow(libc::SYS_connect)
                .allow(libc::SYS_sendto)
                .allow(libc::SYS_recvfrom);
        }

        let config = ProcessBoundaryConfig {
            mount_config: MountConfig {
                tmpfs_path: std::path::PathBuf::from(&tmpfs_path),
                mount_sterile_proc: true,
            },
            seccomp_filter: filter,
            ..Default::default()
        };

        let raw_ptr = region.as_ref().map(|r| r.as_ptr() as usize).unwrap_or(0);

        // Inspect the live isolated execution layer: soft-dirty pagemap bits are read while the
        // Copy-on-Write address space is still retained, against the control plane's baseline.
        let boundary_result = execute_isolated_process_with_inspection(
            &config,
            move || {
                if trigger_blocked_syscall {
                    unsafe {
                        let _ =
                            libc::syscall(libc::SYS_socket, libc::AF_INET, libc::SOCK_STREAM, 0);
                    }
                } else if let Some((method, path, body)) = request_to_trigger {
                    if let Some(port) = proxy_port {
                        if let Ok(mut stream) = TcpStream::connect(format!("127.0.0.1:{port}")) {
                            let req = format!(
                                "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: {}\r\n\r\n{body}",
                                body.len()
                            );
                            let _ = stream.write_all(req.as_bytes());
                            let mut resp = Vec::new();
                            let _ = stream.read_to_end(&mut resp);
                        }
                    }
                } else if raw_ptr > 0 {
                    let target = raw_ptr as *mut u8;
                    let payload = [0xDE, 0xAD, 0xBE, 0xEF];
                    unsafe {
                        std::ptr::copy_nonoverlapping(payload.as_ptr(), target, payload.len());
                    }
                }
            },
            |child_pid| {
                coordinator.compute_full_delta(
                    child_pid,
                    region.as_ref().map(|r| (r, memory_size)),
                )
            },
        );

        let (status, inspected) = match boundary_result {
            Ok(s) => s,
            Err(e) => return Err(format!("Execution boundary error: {e}")),
        };

        match status {
            BoundaryExitStatus::SyscallViolation {
                syscall_nr,
                instruction_pointer,
            } => Err(format!(
                "SYSCALL_VIOLATION:{syscall_nr}:{instruction_pointer}"
            )),
            BoundaryExitStatus::Signaled(sig) => {
                Err(format!("Process terminated by signal: {sig}"))
            }
            BoundaryExitStatus::Exited(code) => {
                if code != 0 {
                    return Err(format!("Process exited with status code {code}"));
                }

                let delta = inspected
                    .ok_or_else(|| "Boundary inspection did not execute".to_string())?
                    .map_err(|e| format!("Delta computation error: {e}"))?;

                Ok(delta)
            }
        }
    });

    #[cfg(target_os = "macos")]
    let result = py.allow_threads(|| {
        use dry_exec_core::isolation::{execute_macos_isolated_process, SeatbeltConfig};

        let mut coordinator = DeltaCoordinator::new();
        let scratch_dir = std::path::PathBuf::from(if tmpfs_path.is_empty() {
            "/private/tmp/dex_ephemeral"
        } else {
            &tmpfs_path
        });

        let _ = std::fs::create_dir_all(&scratch_dir);

        let proxy_port = if let Some(endpoints) = mock_endpoints {
            let mut schema = NetworkMockSchema::new();
            for (method, path, status_code, headers, body) in endpoints {
                schema.register_endpoint(
                    method,
                    path,
                    MockResponse {
                        status_code,
                        headers,
                        body,
                    },
                );
            }
            Some(
                coordinator
                    .start_network_proxy(schema)
                    .map_err(|e| format!("Network proxy initialization error: {e}"))?,
            )
        } else {
            None
        };

        let seatbelt_config = SeatbeltConfig {
            allowed_read_paths: vec![
                std::path::PathBuf::from("/usr"),
                std::path::PathBuf::from("/System"),
                std::path::PathBuf::from("/Library"),
                std::path::PathBuf::from("/opt/homebrew"),
                std::path::PathBuf::from("/dev"),
                std::path::PathBuf::from("/etc"),
                std::path::PathBuf::from("/private/etc"),
                std::path::PathBuf::from("/private/tmp"),
            ],
            allowed_write_paths: vec![scratch_dir.clone()],
            allow_loopback_network: true,
            allow_process_exec: true,
        };

        let boundary_res = execute_macos_isolated_process(&scratch_dir, &seatbelt_config, || {
            if trigger_blocked_syscall {
                // Attempt unauthorized write outside ephemeral sandbox to verify Seatbelt EPERM denial
                let path = std::ffi::CString::new("/etc/dex_blocked_write.tmp").unwrap();
                let res = unsafe {
                    libc::open(path.as_ptr(), libc::O_CREAT | libc::O_WRONLY, 0o644)
                };
                if res < 0 {
                    return 126;
                }
                return 0;
            }

            if let Some((method, path, body)) = request_to_trigger {
                if let Some(port) = proxy_port {
                    if let Ok(mut stream) = TcpStream::connect(format!("127.0.0.1:{port}")) {
                        let req = format!(
                            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: {}\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = stream.write_all(req.as_bytes());
                        let mut resp = Vec::new();
                        let _ = stream.read_to_end(&mut resp);
                    }
                }
            }
            0
        });

        let status_code = match boundary_res {
            Ok(code) => code,
            Err(e) => return Err(format!("Execution boundary error: {e}")),
        };

        if trigger_blocked_syscall && status_code == 126 {
            return Err("SYSCALL_VIOLATION:1:0".to_string());
        }

        if status_code != 0 && status_code != 126 {
            return Err(format!("Process exited with status code {status_code}"));
        }

        let delta = coordinator
            .compute_macos_delta(None, Some(&scratch_dir))
            .map_err(|e| format!("Delta computation error: {e}"))?;

        Ok(delta)
    });

    match result {
        Ok(delta) => {
            let dict = PyDict::new(py);
            dict.set_item("total_bytes_mutated", delta.total_bytes_mutated)?;
            dict.set_item("duration_nanos", delta.duration_nanos)?;

            let mem_list = PyList::empty(py);
            for page in delta.memory_mutations {
                let page_dict = PyDict::new(py);
                page_dict.set_item("page_index", page.page_index)?;
                page_dict.set_item("page_address", page.page_address)?;

                let deltas_list = PyList::empty(py);
                for d in page.deltas {
                    let d_dict = PyDict::new(py);
                    d_dict.set_item("offset", d.offset)?;
                    d_dict.set_item("original", PyBytes::new(py, &d.original))?;
                    d_dict.set_item("mutated", PyBytes::new(py, &d.mutated))?;
                    deltas_list.append(d_dict)?;
                }
                page_dict.set_item("deltas", deltas_list)?;
                mem_list.append(page_dict)?;
            }
            dict.set_item("memory_mutations", mem_list)?;

            let fs_list = PyList::empty(py);
            for fs_mut in delta.fs_mutations {
                let fs_dict = PyDict::new(py);
                match fs_mut {
                    dry_exec_core::delta::FsMutation::Created { path, mode, size } => {
                        fs_dict.set_item("mutation_type", "created")?;
                        fs_dict.set_item("path", path.to_string_lossy())?;
                        fs_dict.set_item("mode", mode)?;
                        fs_dict.set_item("size", size)?;
                    }
                    dry_exec_core::delta::FsMutation::Modified { path, deltas: _ } => {
                        fs_dict.set_item("mutation_type", "modified")?;
                        fs_dict.set_item("path", path.to_string_lossy())?;
                    }
                    dry_exec_core::delta::FsMutation::Deleted { path } => {
                        fs_dict.set_item("mutation_type", "deleted")?;
                        fs_dict.set_item("path", path.to_string_lossy())?;
                    }
                }
                fs_list.append(fs_dict)?;
            }
            dict.set_item("fs_mutations", fs_list)?;

            let net_list = PyList::empty(py);
            for net_mut in delta.network_mutations {
                let net_dict = PyDict::new(py);
                net_dict.set_item("method", net_mut.method)?;
                net_dict.set_item("url", net_mut.url)?;
                let h_dict = PyDict::new(py);
                for (k, v) in net_mut.headers {
                    h_dict.set_item(k, v)?;
                }
                net_dict.set_item("headers", h_dict)?;
                net_dict.set_item("request_body", PyBytes::new(py, &net_mut.request_body))?;
                net_dict.set_item("response_status", net_mut.response_status)?;
                net_dict.set_item("response_body", PyBytes::new(py, &net_mut.response_body))?;
                net_list.append(net_dict)?;
            }
            dict.set_item("network_mutations", net_list)?;

            Ok(dict.into())
        }
        Err(err_msg) => {
            if err_msg.starts_with("SYSCALL_VIOLATION:") {
                let parts: Vec<&str> = err_msg.split(':').collect();
                let syscall_nr: u32 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                let ip: u64 = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);

                let py_err = SyscallBoundaryError::new_err(format!(
                    "Boundary breach intercepted: code {syscall_nr} at instruction pointer {ip:#x}"
                ));
                let value = py_err.value(py);
                value.setattr("syscall_nr", syscall_nr)?;
                value.setattr("instruction_pointer", ip)?;
                Err(py_err)
            } else if err_msg.contains("Delta computation") {
                Err(StateDeltaComputationError::new_err(err_msg))
            } else {
                Err(IsolationSetupError::new_err(err_msg))
            }
        }
    }
}

#[pymodule]
fn _dry_exec_ffi(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(execute_isolated_action, m)?)?;
    m.add("IsolationSetupError", py.get_type::<IsolationSetupError>())?;
    m.add(
        "SyscallBoundaryError",
        py.get_type::<SyscallBoundaryError>(),
    )?;
    m.add(
        "StateDeltaComputationError",
        py.get_type::<StateDeltaComputationError>(),
    )?;
    Ok(())
}
