//! Ephemeral execution boundary and kernel isolation module.

#[cfg(target_os = "linux")]
pub mod mount;
#[cfg(target_os = "linux")]
pub mod namespace;
#[cfg(target_os = "linux")]
pub mod process;
#[cfg(target_os = "linux")]
pub mod seccomp;

#[cfg(target_os = "linux")]
pub use mount::{mount_ephemeral_tmpfs, set_mount_propagation_private, MountConfig};
#[cfg(target_os = "linux")]
pub use namespace::{bring_loopback_up, NamespaceFlags};
#[cfg(target_os = "linux")]
pub use process::{
    execute_isolated_process, execute_isolated_process_with_inspection, BoundaryContext,
    ProcessBoundaryConfig,
};
#[cfg(target_os = "linux")]
pub use seccomp::{SeccompFilter, SyscallAction};

#[cfg(target_os = "macos")]
pub mod macos_process;
#[cfg(target_os = "macos")]
pub mod macos_seatbelt;

#[cfg(target_os = "macos")]
pub use macos_process::execute_macos_isolated_process;
#[cfg(target_os = "macos")]
pub use macos_seatbelt::{apply_seatbelt_profile, generate_seatbelt_profile, SeatbeltConfig};
