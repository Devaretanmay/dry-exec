//! Type-safe error and status representations for the dry-exec execution layer.

use thiserror::Error;

/// Type-safe error variants encountered along the ephemeral execution boundary.
#[derive(Debug, Error)]
pub enum IsolationError {
    #[error("Namespace isolation failure: {0}")]
    NamespaceFailure(String),

    #[error("Mount boundary configuration failure: {0}")]
    MountFailure(String),

    #[error("Seccomp-BPF syscall interception filter failure: {0}")]
    SeccompFailure(String),

    #[error("Synchronization channel error: {0}")]
    SyncError(String),

    #[error("Process clone or execution layer failure: {0}")]
    ProcessFailure(String),

    #[error("State delta engine error: {0}")]
    DeltaError(#[from] DeltaError),

    #[error("Kernel syscall failure: {0}")]
    SyscallError(#[from] nix::errno::Errno),
}

/// Type-safe error variants encountered during state delta computation.
#[derive(Debug, Error)]
pub enum DeltaError {
    #[error("Memory mapping failure: {0}")]
    MmapFailure(String),

    #[error("Kernel pagemap parsing error: {0}")]
    PagemapParseError(String),

    #[error("Process memory access error (pid {pid}): {reason}")]
    ProcessMemoryError { pid: i32, reason: String },

    #[error("Filesystem snapshot or diff failure: {0}")]
    FsDiffError(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

/// Resulting execution boundary exit status returned to the control flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundaryExitStatus {
    /// Isolated process completed execution with status code.
    Exited(i32),

    /// Isolated process was terminated by an external signal.
    Signaled(i32),

    /// Intercepted syscall breached the boundary, resulting in deterministic SIGSYS interception.
    SyscallViolation {
        syscall_nr: u32,
        instruction_pointer: u64,
    },
}
