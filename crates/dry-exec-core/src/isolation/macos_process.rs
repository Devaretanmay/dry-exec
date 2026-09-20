//! macOS isolated child process supervisor with Seatbelt kernel enforcement.

use crate::error::IsolationError;
use crate::isolation::macos_seatbelt::{
    apply_seatbelt_profile, generate_seatbelt_profile, SeatbeltConfig,
};
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
