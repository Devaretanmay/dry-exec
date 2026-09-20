//! Ephemeral mount namespace configuration and tmpfs overlay primitives.

use std::path::Path;
use nix::mount::{mount, MsFlags};
use crate::error::IsolationError;

/// Configuration for isolated mount namespaces.
#[derive(Debug, Clone)]
pub struct MountConfig {
    /// Ephemeral tmpfs root directory path.
    pub tmpfs_path: std::path::PathBuf,
    /// Whether to mount a sterile /proc inside the isolated PID namespace.
    pub mount_sterile_proc: bool,
}

impl Default for MountConfig {
    fn default() -> Self {
        Self {
            tmpfs_path: std::path::PathBuf::from("/tmp/dry_exec_ephemeral"),
            mount_sterile_proc: true,
        }
    }
}

/// Enforce private propagation on root filesystem.
pub fn set_mount_propagation_private() -> Result<(), IsolationError> {
    mount(
        None::<&str>,
        "/",
        None::<&str>,
        MsFlags::MS_REC | MsFlags::MS_PRIVATE,
        None::<&str>,
    )
    .map_err(|e| {
        IsolationError::MountFailure(format!("Failed to set MS_REC | MS_PRIVATE on root: {e}"))
    })
}

/// Mount ephemeral tmpfs storage for isolated state mutations.
pub fn mount_ephemeral_tmpfs(target: &Path) -> Result<(), IsolationError> {
    if !target.exists() {
        std::fs::create_dir_all(target).map_err(|e| {
            IsolationError::MountFailure(format!(
                "Failed to create directory {}: {e}",
                target.display()
            ))
        })?;
    }

    mount(
        Some("tmpfs"),
        target,
        Some("tmpfs"),
        MsFlags::MS_NODEV | MsFlags::MS_NOSUID | MsFlags::MS_NOEXEC,
        Some("size=64m,mode=0700"),
    )
    .map_err(|e| {
        IsolationError::MountFailure(format!(
            "Failed to mount ephemeral tmpfs on {}: {e}",
            target.display()
        ))
    })
}

/// Mount sterile /proc to expose only isolated namespace process IDs.
pub fn mount_sterile_proc() -> Result<(), IsolationError> {
    let proc_path = Path::new("/proc");
    if proc_path.exists() {
        mount(
            Some("proc"),
            proc_path,
            Some("proc"),
            MsFlags::MS_NODEV | MsFlags::MS_NOSUID | MsFlags::MS_NOEXEC,
            None::<&str>,
        )
        .map_err(|e| {
            IsolationError::MountFailure(format!("Failed to mount sterile /proc: {e}"))
        })?;
    }
    Ok(())
}
