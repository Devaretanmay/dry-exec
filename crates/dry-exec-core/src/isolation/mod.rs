//! Ephemeral execution boundary and namespace isolation module.

pub mod mount;
pub mod namespace;
pub mod process;
pub mod seccomp;

pub use mount::{mount_ephemeral_tmpfs, set_mount_propagation_private, MountConfig};
pub use namespace::NamespaceFlags;
pub use process::{execute_isolated_process, ProcessBoundaryConfig};
pub use seccomp::{SeccompFilter, SyscallAction};
