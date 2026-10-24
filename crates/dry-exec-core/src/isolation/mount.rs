//! Ephemeral mount namespace configuration and tmpfs overlay primitives.

use crate::error::IsolationError;
use nix::mount::{mount, MsFlags};
use std::path::{Path, PathBuf};

/// Configuration for isolated mount namespaces.
#[derive(Debug, Clone)]
pub struct MountConfig {
    /// Ephemeral tmpfs root directory path.
    pub tmpfs_path: std::path::PathBuf,
    /// Whether to mount a sterile /proc inside the isolated PID namespace.
    pub mount_sterile_proc: bool,
    /// Host-backed paths that remain writable inside the boundary after the host tree is
    /// remounted read-only. Used by workspace trials; the caller owns their lifecycle.
    pub writable_paths: Vec<std::path::PathBuf>,
}

impl Default for MountConfig {
    fn default() -> Self {
        Self {
            tmpfs_path: std::path::PathBuf::from("/tmp/dry_exec_ephemeral"),
            mount_sterile_proc: true,
            writable_paths: Vec::new(),
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

/// Remount every host-backed filesystem in the isolated mount namespace read-only.
///
/// A mount namespace isolates mount operations, not the files they expose: without this step the
/// host root stays writable and an isolated command can mutate durable state outside the ephemeral
/// root. Pseudo-filesystems and device nodes stay writable so process startup and standard
/// descriptor IO keep working; the ephemeral tmpfs is mounted after this call and remains writable.
pub fn restrict_root_readonly() -> Result<(), IsolationError> {
    let mounts = std::fs::read_to_string("/proc/self/mounts").map_err(|e| {
        IsolationError::MountFailure(format!("Failed to read /proc/self/mounts: {e}"))
    })?;

    for line in mounts.lines() {
        let mut fields = line.split_whitespace();
        let _device = fields.next();
        let Some(raw_mountpoint) = fields.next() else {
            continue;
        };
        let mountpoint = decode_mount_path(raw_mountpoint);

        if mountpoint == Path::new("/dev/shm") || mountpoint == Path::new("/dev/mqueue") {
            // Shared-memory and message-queue mounts are host-backed and must not stay writable.
            let _ = remount_readonly(&mountpoint);
            continue;
        }

        if is_pseudo_mount(&mountpoint) {
            continue;
        }

        remount_readonly(&mountpoint)?;
    }

    Ok(())
}

/// Whether the mountpoint belongs to a pseudo-filesystem that the execution layer needs writable.
fn is_pseudo_mount(path: &Path) -> bool {
    ["/proc", "/sys", "/dev"]
        .iter()
        .any(|root| path == Path::new(root) || path.starts_with(root))
}

/// Decode the octal escapes used by `/proc/self/mounts` for spaces and control characters.
fn decode_mount_path(raw: &str) -> PathBuf {
    let mut decoded = String::with_capacity(raw.len());
    let mut chars = raw.chars();

    while let Some(ch) = chars.next() {
        if ch != '\\' {
            decoded.push(ch);
            continue;
        }

        let digits: String = chars.by_ref().take(3).collect();
        match u8::from_str_radix(&digits, 8) {
            Ok(byte) => decoded.push(byte as char),
            Err(_) => {
                decoded.push('\\');
                decoded.push_str(&digits);
            }
        }
    }

    PathBuf::from(decoded)
}

/// Change an existing mount to read-only without touching the underlying superblock.
fn remount_readonly(path: &Path) -> Result<(), IsolationError> {
    mount(
        None::<&str>,
        path,
        None::<&str>,
        MsFlags::MS_REMOUNT | MsFlags::MS_BIND | MsFlags::MS_RDONLY,
        None::<&str>,
    )
    .map_err(|e| {
        IsolationError::MountFailure(format!(
            "Failed to remount {} read-only: {e}",
            path.display()
        ))
    })
}

/// Restore write access to a host-backed path after [`restrict_root_readonly`].
///
/// A bind mount creates a new mount for the same underlying directory; remounting it without
/// `MS_RDONLY` makes that view writable while every other host mount stays read-only. Callers
/// pass only disposable workspace clones, never a durable target tree.
pub fn bind_mount_writable(path: &Path) -> Result<(), IsolationError> {
    if !path.exists() {
        return Err(IsolationError::MountFailure(format!(
            "Writable workspace path does not exist: {}",
            path.display()
        )));
    }

    mount(
        Some(path),
        path,
        None::<&str>,
        MsFlags::MS_BIND,
        None::<&str>,
    )
    .map_err(|e| {
        IsolationError::MountFailure(format!(
            "Failed to bind writable workspace {}: {e}",
            path.display()
        ))
    })?;

    mount(
        None::<&str>,
        path,
        None::<&str>,
        MsFlags::MS_REMOUNT | MsFlags::MS_BIND,
        None::<&str>,
    )
    .map_err(|e| {
        IsolationError::MountFailure(format!(
            "Failed to remount writable workspace {}: {e}",
            path.display()
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
        .map_err(|e| IsolationError::MountFailure(format!("Failed to mount sterile /proc: {e}")))?;
    }
    Ok(())
}
