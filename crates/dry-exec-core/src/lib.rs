//! dry-exec: Ephemeral execution boundary primitive and deterministic state delta engine.

#[cfg(all(not(target_os = "linux"), not(target_os = "macos")))]
compile_error!(
    "dry-exec requires Linux kernel primitives (namespaces, seccomp-bpf, soft-dirty pagemap) or macOS kernel primitives (Seatbelt, APFS clonefile)."
);

pub mod delta;
pub mod error;
pub mod isolation;

pub use delta::{
    AnonymousMemoryRegion, ByteDelta, DeltaCoordinator, FsMutation, InterceptedRequest,
    MockResponse, NetworkBoundary, NetworkMockSchema, PageMutation, StateDelta, TransparentProxy,
};
pub use error::{BoundaryExitStatus, DeltaError, IsolationError};

#[cfg(target_os = "linux")]
pub use isolation::{
    execute_isolated_process, execute_isolated_process_with_inspection, BoundaryContext,
    MountConfig, NamespaceFlags, ProcessBoundaryConfig, SeccompFilter, SyscallAction,
};

#[cfg(target_os = "macos")]
pub use delta::{apfs_clone_directory, compute_macos_fs_delta};
#[cfg(target_os = "macos")]
pub use isolation::{
    apply_seatbelt_profile, execute_macos_isolated_process, generate_seatbelt_profile,
    SeatbeltConfig,
};
