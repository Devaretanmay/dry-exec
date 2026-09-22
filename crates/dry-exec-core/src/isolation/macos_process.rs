//! macOS isolated child process supervisor with Seatbelt kernel enforcement.

use crate::error::IsolationError;
use crate::isolation::macos_seatbelt::{
    apply_seatbelt_profile, generate_seatbelt_profile, SeatbeltConfig,
};
use std::os::unix::io::FromRawFd;
use std::path::Path;

/// Executes an action within a child process locked down by a macOS Seatbelt profile.
pub fn execute_macos_isolated_process<F>(
    _scratch_dir: &Path,
    seatbelt_config: &SeatbeltConfig,
    action: F,
) -> Result<i32, IsolationError>
where
    F: FnOnce() -> i32,
{
    let profile = generate_seatbelt_profile(seatbelt_config);

    unsafe {
        let pid = libc::fork();
        if pid < 0 {
            return Err(IsolationError::ProcessFailure(format!(
                "fork() failed: errno {}",
                std::io::Error::last_os_error()
            )));
        }

        if pid == 0 {
            // Child: lock down process with irreversible Seatbelt profile
            if let Err(e) = apply_seatbelt_profile(&profile) {
                eprintln!("[dex-macos] Seatbelt initialization error: {e}");
                libc::_exit(126);
            }

            // Execute isolated action within kernel boundary
            let status = action();
            libc::_exit(status);
        } else {
            // Parent supervisor: await child termination
            let mut status = 0;
            let res = libc::waitpid(pid, &mut status, 0);
            if res < 0 {
                return Err(IsolationError::ProcessFailure(format!(
                    "waitpid() failed: errno {}",
                    std::io::Error::last_os_error()
                )));
            }

            if libc::WIFEXITED(status) {
                Ok(libc::WEXITSTATUS(status))
            } else if libc::WIFSIGNALED(status) {
                Ok(128 + libc::WTERMSIG(status))
            } else {
                Ok(1)
            }
        }
    }
}

/// Execute a real argv vector directly after Seatbelt installation.
pub fn execute_macos_command(
    scratch_dir: &Path,
    seatbelt_config: &SeatbeltConfig,
    command: &[String],
    proxy_url: Option<&str>,
) -> Result<(i32, Vec<u8>), IsolationError> {
    if command.is_empty() {
        return Err(IsolationError::ProcessFailure(
            "empty command vector".to_string(),
        ));
    }
    let profile = generate_seatbelt_profile(seatbelt_config);
    let argv: Vec<std::ffi::CString> = command
        .iter()
        .map(|part| {
            std::ffi::CString::new(part.as_str()).map_err(|e| {
                IsolationError::ProcessFailure(format!("command contains NUL byte: {e}"))
            })
        })
        .collect::<Result<_, _>>()?;
    let mut argv_ptrs: Vec<*const libc::c_char> = argv.iter().map(|part| part.as_ptr()).collect();
    argv_ptrs.push(std::ptr::null());

    unsafe {
        let mut pipe_fds = [0; 2];
        if libc::pipe(pipe_fds.as_mut_ptr()) != 0 {
            return Err(IsolationError::ProcessFailure(format!(
                "pipe() failed: errno {}",
                std::io::Error::last_os_error()
            )));
        }
        let pid = libc::fork();
        if pid < 0 {
            libc::close(pipe_fds[0]);
            libc::close(pipe_fds[1]);
            return Err(IsolationError::ProcessFailure(format!(
                "fork() failed: errno {}",
                std::io::Error::last_os_error()
            )));
        }
        if pid == 0 {
            libc::close(pipe_fds[0]);
            if libc::dup2(pipe_fds[1], libc::STDOUT_FILENO) < 0
                || libc::dup2(pipe_fds[1], libc::STDERR_FILENO) < 0
            {
                libc::_exit(126);
            }
            libc::close(pipe_fds[1]);
            if let Err(error) = apply_seatbelt_profile(&profile) {
                eprintln!("sandbox_init failed: {error}");
                libc::_exit(126);
            }
            if libc::chdir(
                std::ffi::CString::new(scratch_dir.to_string_lossy().as_bytes())
                    .unwrap()
                    .as_ptr(),
            ) != 0
            {
                eprintln!(
                    "[dex-macos] sandboxed chdir failed: {}",
                    std::io::Error::last_os_error()
                );
                libc::_exit(126);
            }
            if let Some(proxy) = proxy_url {
                let proxy = std::ffi::CString::new(proxy).unwrap();
                for key in [
                    "HTTP_PROXY",
                    "HTTPS_PROXY",
                    "ALL_PROXY",
                    "http_proxy",
                    "https_proxy",
                    "all_proxy",
                ] {
                    let key = std::ffi::CString::new(key).unwrap();
                    libc::setenv(key.as_ptr(), proxy.as_ptr(), 1);
                }
                let key = std::ffi::CString::new("NO_PROXY").unwrap();
                let empty = std::ffi::CString::new("").unwrap();
                libc::setenv(key.as_ptr(), empty.as_ptr(), 1);
            }
            libc::execvp(argv[0].as_ptr(), argv_ptrs.as_ptr());
            libc::perror(c"execvp failed".as_ptr());
            libc::_exit(127);
        }

        libc::close(pipe_fds[1]);
        let reader = std::thread::spawn(move || {
            let mut output = Vec::new();
            let mut pipe = std::fs::File::from_raw_fd(pipe_fds[0]);
            let _ = std::io::Read::read_to_end(&mut pipe, &mut output);
            output
        });
        let mut status = 0;
        if libc::waitpid(pid, &mut status, 0) < 0 {
            return Err(IsolationError::ProcessFailure(format!(
                "waitpid() failed: errno {}",
                std::io::Error::last_os_error()
            )));
        }
        let output = reader.join().unwrap_or_default();
        let exit_code = if libc::WIFEXITED(status) {
            libc::WEXITSTATUS(status)
        } else if libc::WIFSIGNALED(status) {
            128 + libc::WTERMSIG(status)
        } else {
            1
        };
        Ok((exit_code, output))
    }
}
