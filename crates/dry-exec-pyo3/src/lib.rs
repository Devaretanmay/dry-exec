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
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::TcpStream;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Schema-driven mock endpoint: (method, path, status_code, headers, body).
type MockEndpoint = (String, String, u16, HashMap<String, String>, Vec<u8>);

static CAPTURE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// RAII capture file preventing symlink attacks (O_CREAT | O_EXCL with 0o600) and disk leakage on panic/error.
struct TempCaptureFile {
    path: PathBuf,
    file: Option<File>,
}

impl TempCaptureFile {
    fn new(label: &str) -> Result<Self, String> {
        let pid = std::process::id();
        let seq = CAPTURE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| format!("capture clock error: {e}"))?
            .as_nanos();
        let path = std::env::temp_dir().join(format!("dry_exec_{label}_{pid}_{seq}_{nonce}"));
        let mut opts = OpenOptions::new();
        opts.create_new(true).read(true).write(true).mode(0o600);
        let file = opts
            .open(&path)
            .map_err(|e| format!("capture file error: {e}"))?;
        Ok(Self {
            path,
            file: Some(file),
        })
    }

    fn as_raw_fd(&self) -> std::os::unix::io::RawFd {
        self.file.as_ref().map(|f| f.as_raw_fd()).unwrap_or(-1)
    }

    fn read_and_unlink(&mut self) -> Result<Vec<u8>, String> {
        let mut bytes = Vec::new();
        if let Some(mut file) = self.file.take() {
            file.seek(SeekFrom::Start(0))
                .map_err(|e| format!("capture seek error: {e}"))?;
            file.read_to_end(&mut bytes)
                .map_err(|e| format!("capture read error: {e}"))?;
        }
        let _ = std::fs::remove_file(&self.path);
        Ok(bytes)
    }
}

impl Drop for TempCaptureFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// RAII directory used for pre-execution baseline clones.
struct TempDirectory {
    path: PathBuf,
}

impl TempDirectory {
    fn new(label: &str) -> Result<Self, String> {
        let pid = std::process::id();
        let seq = CAPTURE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| format!("snapshot clock error: {e}"))?
            .as_nanos();
        Ok(Self {
            path: std::env::temp_dir().join(format!("dry_exec_{label}_{pid}_{seq}_{nonce}")),
        })
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

create_exception!(_dry_exec_ffi, IsolationSetupError, PyException);
create_exception!(_dry_exec_ffi, SyscallBoundaryError, PyException);
create_exception!(_dry_exec_ffi, StateDeltaComputationError, PyException);
create_exception!(_dry_exec_ffi, SchemaViolationError, PyException);

/// Validate the action on the native side of the FFI boundary.
///
/// Python validates before crossing, but direct FFI callers bypass that gate. Re-checking the
/// required shape and the authorized target set here keeps the boundary fail-closed for every
/// entry point.
fn validate_action_json(
    py: Python<'_>,
    action_json: &str,
    allowed_targets: Option<&[String]>,
) -> PyResult<()> {
    let value: serde_json::Value = serde_json::from_str(action_json).map_err(|error| {
        SchemaViolationError::new_err(format!("Action payload is not valid JSON: {error}"))
    })?;
    let object = value
        .as_object()
        .ok_or_else(|| SchemaViolationError::new_err("Action payload must be a JSON object"))?;

    let required_string = |field: &str| -> PyResult<String> {
        object
            .get(field)
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| {
                SchemaViolationError::new_err(format!(
                    "Action is missing required string field '{field}'"
                ))
            })
    };

    let _action_id = required_string("action_id")?;
    let target_resource = required_string("target_resource")?;
    let _mutation_type = required_string("mutation_type")?;
    if !object
        .get("payload")
        .map(|value| value.is_object())
        .unwrap_or(false)
    {
        return Err(SchemaViolationError::new_err(
            "Action field 'payload' must be a JSON object",
        ));
    }

    if let Some(allowed) = allowed_targets {
        if !allowed
            .iter()
            .any(|candidate| candidate == "*" || candidate == &target_resource)
        {
            let error = SchemaViolationError::new_err(format!(
                "Action targeting resource '{target_resource}' violates the native schema boundary"
            ));
            let value = error.value(py);
            value.setattr("invalid_target", target_resource)?;
            value.setattr("violation_type", "unauthorized_mutation_target")?;
            return Err(error);
        }
    }

    Ok(())
}

#[pyfunction]
// The FFI boundary stays positional: each argument is a distinct facet of the ephemeral
// execution request, and bundling them would complicate every Python caller for no gain.
#[allow(clippy::too_many_arguments)]
#[pyo3(signature = (action_json, memory_size, tmpfs_path, trigger_blocked_syscall, mock_endpoints=None, request_to_trigger=None, decision_thresholds=None, command=None, allowed_targets=None, workspace_root=None, rootless=false))]
fn execute_isolated_action(
    py: Python<'_>,
    action_json: String,
    memory_size: usize,
    tmpfs_path: String,
    trigger_blocked_syscall: bool,
    mock_endpoints: Option<Vec<MockEndpoint>>,
    request_to_trigger: Option<(String, String, String)>,
    decision_thresholds: Option<(usize, usize, f32)>,
    command: Option<Vec<String>>,
    allowed_targets: Option<Vec<String>>,
    workspace_root: Option<String>,
    rootless: bool,
) -> PyResult<PyObject> {
    use dry_exec_core::decision::{evaluate, DeltaSummary, Environment};
    #[cfg(target_os = "linux")]
    use dry_exec_core::delta::AnonymousMemoryRegion;
    use dry_exec_core::delta::{DeltaCoordinator, MockResponse, NetworkMockSchema};
    #[cfg(target_os = "linux")]
    use dry_exec_core::delta::{NetworkBoundary, TransparentProxy};

    validate_action_json(py, &action_json, allowed_targets.as_deref())?;
    #[cfg(target_os = "macos")]
    let _ = (memory_size, rootless);

    // A virtualenv or hosted Python can live outside the system executable directories. Only
    // grant its runtime reads when the command selects this exact interpreter; host application
    // source and unrelated home directories remain outside the read allowance.
    #[cfg(target_os = "macos")]
    let python_runtime = {
        let sys = py.import("sys")?;
        let executable: String = sys.getattr("executable")?.extract()?;
        let executable =
            std::fs::canonicalize(&executable).unwrap_or_else(|_| PathBuf::from(executable));
        let selected = command
            .as_ref()
            .and_then(|argv| argv.first())
            .and_then(|program| std::fs::canonicalize(program).ok());
        if selected.as_ref() == Some(&executable) {
            let mut roots = Vec::new();
            for name in ["prefix", "base_prefix", "exec_prefix", "base_exec_prefix"] {
                let path: String = sys.getattr(name)?.extract()?;
                let path = std::fs::canonicalize(&path).unwrap_or_else(|_| PathBuf::from(path));
                if !roots.contains(&path) {
                    roots.push(path);
                }
            }
            Some((executable, roots))
        } else {
            None
        }
    };

    #[cfg(target_os = "linux")]
    let result = py.allow_threads(|| {
        use dry_exec_core::error::BoundaryExitStatus;
        use dry_exec_core::isolation::{
            execute_isolated_process_with_inspection, landlock_supported, MountConfig,
            NamespaceFlags, ProcessBoundaryConfig, SeccompFilter, SyscallAction,
        };

        if !landlock_supported() {
            return Err(
                "Linux filesystem isolation requires Landlock ABI 3 or newer".to_string(),
            );
        }

        let mut stdout_capture = TempCaptureFile::new("stdout")?;
        let mut stderr_capture = TempCaptureFile::new("stderr")?;
        let mut status_capture = TempCaptureFile::new("status")?;
        let stdout_fd = stdout_capture.as_raw_fd();
        let stderr_fd = stderr_capture.as_raw_fd();
        let status_fd = status_capture.as_raw_fd();
        let command_for_child = command.clone();

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
        let workspace_path = workspace_root.as_ref().map(PathBuf::from);

        // A workspace trial runs against a writable clone of the caller's tree. Snapshot a
        // separate clone so the delta engine retains the exact pre-execution bytes while the
        // child mutates the working clone. Without a workspace the boundary mounts a fresh
        // tmpfs, whose baseline is empty regardless of what the host mountpoint contains.
        let mut baseline_guard: Option<TempDirectory> = None;
        match workspace_path.as_ref() {
            Some(workspace) => {
                let baseline = TempDirectory::new("baseline")?;
                dry_exec_core::delta::clone_directory(workspace, baseline.path())
                    .map_err(|e| format!("Filesystem baseline clone error: {e}"))?;
                coordinator
                    .snapshot_baseline(Some(baseline.path()))
                    .map_err(|e| format!("Filesystem baseline snapshot error: {e}"))?;
                baseline_guard = Some(baseline);
            }
            None => coordinator.snapshot_empty(std::path::Path::new(&tmpfs_path)),
        }

        // Reserve the loopback port for the schema-driven mock listener. The isolated execution
        // layer binds it inside its own network namespace and the control plane serves it, which
        // is the only way requests originating in that namespace can reach the transparent proxy.
        let network_boundary = match mock_endpoints {
            Some(endpoints) => {
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
                let port = TransparentProxy::reserve_loopback_port()
                    .map_err(|e| format!("Network proxy initialization error: {e}"))?;
                Some(NetworkBoundary { port, schema })
            }
            None => None,
        };
        let proxy_port = network_boundary.as_ref().map(|boundary| boundary.port);

        // Arbitrary commands need their runtime syscall surface. Keep the filter attached, while
        // retaining strict TRAP behavior for explicit boundary-probe tests.
        let mut filter = if command_for_child.is_some() {
            let command_filter = SeccompFilter::new(SyscallAction::Allow);
            if trigger_blocked_syscall {
                command_filter.trap(libc::SYS_socket)
            } else {
                command_filter
            }
        } else {
            SeccompFilter::new(SyscallAction::Trap).with_baseline_whitelist()
        };
        if request_to_trigger.is_some() {
            // Outbound request surface for a userspace HTTP client: socket setup, connection,
            // and request/response IO. `fcntl` is included because the standard library's socket
            // types manipulate descriptor flags, and the request cannot complete without it.
            filter = filter
                .allow(libc::SYS_socket)
                .allow(libc::SYS_connect)
                .allow(libc::SYS_sendto)
                .allow(libc::SYS_recvfrom)
                .allow(libc::SYS_fcntl);
        }

        let command_cwd = workspace_path
            .clone()
            .unwrap_or_else(|| std::path::PathBuf::from(&tmpfs_path));
        let workspace_for_delta = workspace_path.clone();

        let namespaces = if rootless {
            NamespaceFlags::default() | NamespaceFlags::USER
        } else {
            NamespaceFlags::default()
        };

        let config = ProcessBoundaryConfig {
            namespaces,
            mount_config: MountConfig {
                tmpfs_path: std::path::PathBuf::from(&tmpfs_path),
                mount_sterile_proc: true,
                writable_paths: workspace_path.iter().cloned().collect(),
            },
            seccomp_filter: filter,
            landlock: true,
            network_boundary,
            ..Default::default()
        };

        let raw_ptr = region.as_ref().map(|r| r.as_ptr() as usize).unwrap_or(0);

        // Inspect the live isolated execution layer: soft-dirty pagemap bits are read while the
        // Copy-on-Write address space is still retained, against the control plane's baseline.
        let boundary_result = execute_isolated_process_with_inspection(
            &config,
            move || {
                if let Some(argv) = command_for_child {
                    if let Some(program) = argv.first() {
                        let mut process = Command::new(program);
                        process.args(&argv[1..]).current_dir(&command_cwd);
                        if let Some(port) = proxy_port {
                            let proxy = format!("http://127.0.0.1:{port}");
                            process
                                .env("HTTP_PROXY", &proxy)
                                .env("HTTPS_PROXY", &proxy)
                                .env("ALL_PROXY", &proxy)
                                .env("http_proxy", &proxy)
                                .env("https_proxy", &proxy)
                                .env("all_proxy", &proxy)
                                .env("NO_PROXY", "");
                        }
                        let output = process.output();
                        let (stdout, stderr, exit_code) = match output {
                            Ok(output) => (
                                output.stdout,
                                output.stderr,
                                output.status.code().unwrap_or(128),
                            ),
                            Err(error) => (
                                Vec::new(),
                                format!("failed to execute command: {error}\n").into_bytes(),
                                127,
                            ),
                        };
                        unsafe {
                            let mut out = std::mem::ManuallyDrop::new(File::from_raw_fd(stdout_fd));
                            let mut err = std::mem::ManuallyDrop::new(File::from_raw_fd(stderr_fd));
                            let mut status_file = std::mem::ManuallyDrop::new(File::from_raw_fd(status_fd));
                            let _ = out.write_all(&stdout);
                            let _ = err.write_all(&stderr);
                            let _ = write!(&mut *status_file, "{exit_code}");
                            let _ = out.flush();
                            let _ = err.flush();
                        }
                    }
                } else if trigger_blocked_syscall {
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
            |context| {
                // Adopt the proxy serving the isolated namespace so intercepted network mutations
                // are aggregated into the state delta alongside memory and filesystem mutations.
                if let Some(proxy) = context.network {
                    coordinator.set_network_proxy(proxy);
                }
                coordinator.compute_full_delta_at(
                    context.child_pid,
                    region.as_ref().map(|r| (r, memory_size)),
                    workspace_for_delta.as_deref(),
                )
            },
        );

        let (status, inspected) = match boundary_result {
            Ok(s) => s,
            Err(e) => return Err(format!("Execution boundary error: {e}")),
        };
        drop(baseline_guard);

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

                let mut delta = inspected
                    .ok_or_else(|| "Boundary inspection did not execute".to_string())?
                    .map_err(|e| format!("Delta computation error: {e}"))?;

                delta.stdout = stdout_capture.read_and_unlink()?;
                delta.stderr = stderr_capture.read_and_unlink()?;
                let status_bytes = status_capture.read_and_unlink()?;
                delta.exit_code = String::from_utf8_lossy(&status_bytes)
                    .trim()
                    .parse()
                    .unwrap_or(127);

                Ok(delta)
            }
        }
    });

    #[cfg(target_os = "macos")]
    let result = py.allow_threads(|| {
        use dry_exec_core::isolation::{
            execute_macos_command, execute_macos_isolated_process, SeatbeltConfig,
        };

        let mut stdout_capture = TempCaptureFile::new("stdout")?;
        let mut stderr_capture = TempCaptureFile::new("stderr")?;
        let stdout_fd = stdout_capture.as_raw_fd();
        let stderr_fd = stderr_capture.as_raw_fd();
        let command_for_child = command.clone();

        let mut coordinator = DeltaCoordinator::new();
        let requested_scratch_dir = workspace_root
            .as_ref()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::PathBuf::from(if tmpfs_path.is_empty() {
                    "/private/tmp/dex_ephemeral"
                } else {
                    &tmpfs_path
                })
            });

        let _ = std::fs::create_dir_all(&requested_scratch_dir);
        let scratch_dir = std::fs::canonicalize(&requested_scratch_dir)
            .unwrap_or(requested_scratch_dir);

        // Snapshot the scratch root before execution so later deltas report modifications and
        // deletions, not just files that happen to remain at the end of the trial.
        let baseline_dir = TempDirectory::new("baseline")?;
        dry_exec_core::delta::clone_directory(&scratch_dir, baseline_dir.path())
            .map_err(|e| format!("Filesystem baseline snapshot error: {e}"))?;

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

        let mut allowed_write_paths = vec![scratch_dir.clone()];
        if !tmpfs_path.is_empty() {
            let ephemeral_root = std::path::PathBuf::from(&tmpfs_path);
            if ephemeral_root != scratch_dir {
                allowed_write_paths.push(ephemeral_root);
            }
        }
        let mut seatbelt_config = SeatbeltConfig {
            allowed_write_paths,
            loopback_proxy_port: proxy_port,
            ..SeatbeltConfig::default()
        };
        if let Some((executable, roots)) = &python_runtime {
            seatbelt_config.allowed_exec_paths.push(executable.clone());
            seatbelt_config.allowed_read_paths.extend(roots.iter().cloned());
        }

        let proxy_url = proxy_port.map(|port| format!("http://127.0.0.1:{port}"));
        let boundary_res = if let Some(argv) = command.as_ref() {
            execute_macos_command(
                &scratch_dir,
                &seatbelt_config,
                argv,
                proxy_url.as_deref(),
            )
            .map(|(code, stdout, stderr)| (code, Some((stdout, stderr))))
        } else {
            execute_macos_isolated_process(&scratch_dir, &seatbelt_config, || {
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

            if let Some(argv) = command_for_child {
                    if let Some(program) = argv.first() {
                    let mut process = Command::new(program);
                    process.args(&argv[1..]).current_dir(&scratch_dir);
                    if let Some(port) = proxy_port {
                        let proxy = format!("http://127.0.0.1:{port}");
                        process
                            .env("HTTP_PROXY", &proxy)
                            .env("HTTPS_PROXY", &proxy)
                            .env("ALL_PROXY", &proxy)
                            .env("http_proxy", &proxy)
                            .env("https_proxy", &proxy)
                            .env("all_proxy", &proxy)
                            .env("NO_PROXY", "");
                    }
                    let output = process.output();
                    let (stdout, stderr, exit_code) = match output {
                        Ok(output) => (
                            output.stdout,
                            output.stderr,
                            output.status.code().unwrap_or(128),
                        ),
                        Err(error) => (
                            Vec::new(),
                            format!("failed to execute command: {error}\n").into_bytes(),
                            127,
                        ),
                    };
                    unsafe {
                        let mut out = std::mem::ManuallyDrop::new(File::from_raw_fd(stdout_fd));
                        let mut err = std::mem::ManuallyDrop::new(File::from_raw_fd(stderr_fd));
                        let _ = out.write_all(&stdout);
                        let _ = err.write_all(&stderr);
                        let _ = out.flush();
                        let _ = err.flush();
                    }
                    return exit_code;
                }
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
            })
            .map(|code| (code, None))
        };

        let (status_code, macos_output) = match boundary_res {
            Ok(result) => result,
            Err(e) => return Err(format!("Execution boundary error: {e}")),
        };

        if trigger_blocked_syscall && status_code == 126 {
            return Err("SYSCALL_VIOLATION:1:0".to_string());
        }

        if command.is_none() && status_code != 0 && status_code != 126 {
            return Err(format!("Process exited with status code {status_code}"));
        }

        let mut delta = coordinator
            .compute_macos_delta(Some(baseline_dir.path()), Some(&scratch_dir))
            .map_err(|e| format!("Delta computation error: {e}"))?;

        if command.is_some() {
            let (stdout, stderr) = macos_output.unwrap_or_default();
            delta.stdout = stdout;
            delta.stderr = stderr;
            let _ = stdout_capture.read_and_unlink();
            let _ = stderr_capture.read_and_unlink();
            delta.exit_code = status_code;
        }

        Ok(delta)
    });

    match result {
        Ok(delta) => {
            // System-One decision layer: the receipt is computed from the scalar summary of the
            // state delta and the calibrated thresholds, in this single FFI crossing.
            let decision_environment = decision_thresholds
                .map(
                    |(max_mutated_bytes, max_network_calls, max_risk_threshold)| Environment {
                        max_mutated_bytes,
                        max_network_calls,
                        max_risk_threshold,
                    },
                )
                .unwrap_or_default();
            let receipt = evaluate(&DeltaSummary::from(&delta), &decision_environment);

            let dict = PyDict::new(py);
            dict.set_item("total_bytes_mutated", delta.total_bytes_mutated)?;
            dict.set_item("schema_breaches", delta.schema_breaches)?;
            let hazard_count = delta
                .fs_mutations
                .iter()
                .filter(|m| m.is_deferred_execution_hazard())
                .count();
            dict.set_item("deferred_execution_hazards", hazard_count)?;
            dict.set_item("duration_nanos", delta.duration_nanos)?;
            dict.set_item("stdout", PyBytes::new(py, &delta.stdout))?;
            dict.set_item("stderr", PyBytes::new(py, &delta.stderr))?;
            dict.set_item("exit_code", delta.exit_code)?;

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
                fs_dict.set_item(
                    "is_deferred_execution_hazard",
                    fs_mut.is_deferred_execution_hazard(),
                )?;
                match fs_mut {
                    dry_exec_core::delta::FsMutation::Created { path, mode, size } => {
                        fs_dict.set_item("mutation_type", "created")?;
                        fs_dict.set_item("path", path.to_string_lossy())?;
                        fs_dict.set_item("mode", mode)?;
                        fs_dict.set_item("size", size)?;
                    }
                    dry_exec_core::delta::FsMutation::Modified { path, deltas } => {
                        fs_dict.set_item("mutation_type", "modified")?;
                        fs_dict.set_item("path", path.to_string_lossy())?;
                        let deltas_list = PyList::empty(py);
                        for byte_delta in deltas {
                            let delta_dict = PyDict::new(py);
                            delta_dict.set_item("offset", byte_delta.offset)?;
                            delta_dict
                                .set_item("original", PyBytes::new(py, &byte_delta.original))?;
                            delta_dict
                                .set_item("mutated", PyBytes::new(py, &byte_delta.mutated))?;
                            deltas_list.append(delta_dict)?;
                        }
                        fs_dict.set_item("deltas", deltas_list)?;
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

            let decision_dict = PyDict::new(py);
            decision_dict.set_item("choice", receipt.choice.as_str())?;
            decision_dict.set_item("risk_score", receipt.risk_score.value())?;
            decision_dict.set_item("noul_trigger", receipt.noul_trigger.as_str())?;
            decision_dict.set_item("reason", receipt.reason)?;
            dict.set_item("decision", decision_dict)?;

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

/// Report native isolation capabilities without executing an action.
#[pyfunction]
fn isolation_capabilities() -> (bool, bool) {
    #[cfg(target_os = "linux")]
    {
        (
            dry_exec_core::isolation::landlock_supported(),
            dry_exec_core::isolation::user_namespace_supported(),
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        (false, false)
    }
}

/// Clone a directory tree using the platform's copy-on-write primitive.
#[pyfunction]
fn clone_directory(src: String, dst: String) -> PyResult<()> {
    dry_exec_core::delta::clone_directory(std::path::Path::new(&src), std::path::Path::new(&dst))
        .map_err(|e| IsolationSetupError::new_err(format!("Workspace clone error: {e}")))
}

#[pymodule]
fn _dry_exec_ffi(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(execute_isolated_action, m)?)?;
    m.add_function(wrap_pyfunction!(isolation_capabilities, m)?)?;
    m.add_function(wrap_pyfunction!(clone_directory, m)?)?;
    m.add("IsolationSetupError", py.get_type::<IsolationSetupError>())?;
    m.add(
        "SyscallBoundaryError",
        py.get_type::<SyscallBoundaryError>(),
    )?;
    m.add(
        "StateDeltaComputationError",
        py.get_type::<StateDeltaComputationError>(),
    )?;
    m.add(
        "SchemaViolationError",
        py.get_type::<SchemaViolationError>(),
    )?;
    Ok(())
}
