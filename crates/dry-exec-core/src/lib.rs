//! dry-exec: Ephemeral execution boundary primitive and deterministic state delta engine.

#[cfg(not(target_os = "linux"))]
compile_error!(
    "dry-exec requires Linux kernel primitives (namespaces, seccomp-bpf, soft-dirty pagemap). Build and execute tests inside the provided Linux container verification harness."
);

#[cfg(target_os = "linux")]
pub mod delta;
#[cfg(target_os = "linux")]
pub mod error;
#[cfg(target_os = "linux")]
pub mod isolation;

#[cfg(target_os = "linux")]
pub use delta::{
    AnonymousMemoryRegion, ByteDelta, DeltaCoordinator, FsMutation, InterceptedRequest,
    MockResponse, NetworkMockSchema, PageMutation, StateDelta, TransparentProxy,
};
#[cfg(target_os = "linux")]
pub use error::{BoundaryExitStatus, DeltaError, IsolationError};
#[cfg(target_os = "linux")]
pub use isolation::{
    execute_isolated_process, MountConfig, NamespaceFlags, ProcessBoundaryConfig, SeccompFilter,
    SyscallAction,
};
