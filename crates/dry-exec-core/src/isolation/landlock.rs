//! Linux Landlock filesystem write containment.
//!
//! The mount namespace makes host-backed mounts read-only, but a process retaining
//! `CAP_SYS_ADMIN` can remount them writable inside its own namespace. Landlock enforces the
//! write boundary at path resolution time, independent of mount flags and symlink retargeting.

use crate::error::IsolationError;
use std::fs::File;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;

const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1;
const LANDLOCK_RULE_PATH_BENEATH: u32 = 1;

const ACCESS_FS_WRITE_FILE: u64 = 1 << 1;
const ACCESS_FS_REMOVE_DIR: u64 = 1 << 4;
const ACCESS_FS_REMOVE_FILE: u64 = 1 << 5;
const ACCESS_FS_MAKE_CHAR: u64 = 1 << 6;
const ACCESS_FS_MAKE_DIR: u64 = 1 << 7;
const ACCESS_FS_MAKE_REG: u64 = 1 << 8;
const ACCESS_FS_MAKE_SOCK: u64 = 1 << 9;
const ACCESS_FS_MAKE_FIFO: u64 = 1 << 10;
const ACCESS_FS_MAKE_BLOCK: u64 = 1 << 11;
const ACCESS_FS_MAKE_SYM: u64 = 1 << 12;
const ACCESS_FS_REFER: u64 = 1 << 13;
const ACCESS_FS_TRUNCATE: u64 = 1 << 14;

#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
}

#[repr(C, packed)]
struct PathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

/// Query the Landlock ABI implemented by the running kernel.
///
/// Returns `None` when Landlock is unavailable or disabled for the calling process.
pub fn landlock_abi() -> Option<u8> {
    let ret = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<RulesetAttr>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    if ret < 0 {
        None
    } else {
        Some(ret as u8)
    }
}

/// Whether the kernel supports the Landlock filesystem boundary.
pub fn landlock_supported() -> bool {
    // Earlier ABIs do not mediate truncate(2), so they cannot enforce this write boundary.
    landlock_abi().is_some_and(|abi| abi >= 3)
}

fn write_access(abi: u8) -> u64 {
    let mut access = ACCESS_FS_WRITE_FILE
        | ACCESS_FS_REMOVE_DIR
        | ACCESS_FS_REMOVE_FILE
        | ACCESS_FS_MAKE_CHAR
        | ACCESS_FS_MAKE_DIR
        | ACCESS_FS_MAKE_REG
        | ACCESS_FS_MAKE_SOCK
        | ACCESS_FS_MAKE_FIFO
        | ACCESS_FS_MAKE_BLOCK
        | ACCESS_FS_MAKE_SYM;

    // `REFER` and `TRUNCATE` were introduced by ABI 2 and ABI 3 respectively.
    if abi >= 2 {
        access |= ACCESS_FS_REFER;
    }
    if abi >= 3 {
        access |= ACCESS_FS_TRUNCATE;
    }

    access
}

fn open_path(path: &Path) -> Result<File, IsolationError> {
    File::options()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_CLOEXEC)
        .open(path)
        .map_err(|error| {
            IsolationError::LandlockFailure(format!(
                "Failed to open Landlock path {}: {error}",
                path.display()
            ))
        })
}

fn add_path_rule(ruleset: &OwnedFd, path: &Path, access: u64) -> Result<(), IsolationError> {
    let file = open_path(path)?;
    let metadata = file.metadata().map_err(|error| {
        IsolationError::LandlockFailure(format!(
            "Failed to inspect Landlock path {}: {error}",
            path.display()
        ))
    })?;

    // A file rule accepts only file-scoped rights; directory rights are invalid for files.
    let allowed_access = if metadata.is_dir() {
        access
    } else {
        access & (ACCESS_FS_WRITE_FILE | ACCESS_FS_TRUNCATE)
    };

    let attr = PathBeneathAttr {
        allowed_access,
        parent_fd: file.as_raw_fd(),
    };

    let ret = unsafe {
        libc::syscall(
            libc::SYS_landlock_add_rule,
            ruleset.as_raw_fd(),
            LANDLOCK_RULE_PATH_BENEATH,
            &attr as *const PathBeneathAttr,
            0u32,
        )
    };
    if ret != 0 {
        return Err(IsolationError::LandlockFailure(format!(
            "Failed to add Landlock rule for {}: errno {}",
            path.display(),
            std::io::Error::last_os_error()
        )));
    }

    Ok(())
}

/// Apply a kernel-enforced write boundary confined to `write_paths`.
///
/// Reads and execution are unaffected; callers are responsible for supplying every directory
/// and file the isolated action is allowed to mutate. The restriction is irreversible for the
/// calling process and all descendants.
pub fn apply_write_containment(write_paths: &[&Path]) -> Result<(), IsolationError> {
    let abi = landlock_abi().ok_or_else(|| {
        IsolationError::LandlockFailure(
            "Landlock filesystem containment is unavailable on this kernel".to_string(),
        )
    })?;
    if abi < 3 {
        return Err(IsolationError::LandlockFailure(
            "Landlock ABI 3 or newer is required to contain file truncation".to_string(),
        ));
    }
    let access = write_access(abi);
    let attr = RulesetAttr {
        handled_access_fs: access,
    };

    let raw_fd = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            &attr as *const RulesetAttr,
            std::mem::size_of::<RulesetAttr>(),
            0u32,
        )
    };
    if raw_fd < 0 {
        return Err(IsolationError::LandlockFailure(format!(
            "Failed to create Landlock ruleset: errno {}",
            std::io::Error::last_os_error()
        )));
    }
    let ruleset = unsafe { OwnedFd::from_raw_fd(raw_fd as i32) };

    for path in write_paths {
        if path.exists() {
            add_path_rule(&ruleset, path, access)?;
        }
    }

    let ret = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
    if ret != 0 {
        return Err(IsolationError::LandlockFailure(format!(
            "Failed to set PR_SET_NO_NEW_PRIVS: errno {}",
            std::io::Error::last_os_error()
        )));
    }

    let ret = unsafe { libc::syscall(libc::SYS_landlock_restrict_self, ruleset.as_raw_fd(), 0u32) };
    if ret != 0 {
        return Err(IsolationError::LandlockFailure(format!(
            "Failed to apply Landlock ruleset: errno {}",
            std::io::Error::last_os_error()
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_access_tracks_kernel_abi() {
        assert_eq!(
            write_access(1) & ACCESS_FS_REFER,
            0,
            "ABI 1 must not request REFER"
        );
        assert_eq!(
            write_access(1) & ACCESS_FS_TRUNCATE,
            0,
            "ABI 1 must not request TRUNCATE"
        );
        assert_ne!(write_access(2) & ACCESS_FS_REFER, 0);
        assert_eq!(write_access(2) & ACCESS_FS_TRUNCATE, 0);
        assert_ne!(write_access(3) & ACCESS_FS_TRUNCATE, 0);
    }
}
