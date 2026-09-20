"""Type-safe exception hierarchy for the dry-exec ephemeral execution boundary."""

from typing import Optional


class DryExecError(Exception):
    """Base exception for all errors encountered along the execution boundary."""
    pass


class SchemaViolationError(DryExecError):
    """Raised synchronously in the Python layer when a proposed action breaches environment boundary schemas.
    
    The Rust FFI primitive is never invoked when this error is raised.
    """
    def __init__(self, message: str, invalid_target: str, violation_type: str):
        super().__init__(message)
        self.invalid_target = invalid_target
        self.violation_type = violation_type


class IsolationSetupError(DryExecError):
    """Raised when the kernel execution layer fails during namespace or mount configuration."""
    pass


class SyscallBoundaryError(DryExecError):
    """Raised when an isolated process executes an unpermitted syscall intercepted by the seccomp boundary.
    
    Provides deterministic telemetry (syscall number and instruction pointer) to enable the
    autonomous execution loop to adapt its control flow.
    """
    def __init__(self, message: str, syscall_nr: int, instruction_pointer: int):
        super().__init__(message)
        self.syscall_nr = syscall_nr
        self.instruction_pointer = instruction_pointer

    def __repr__(self) -> str:
        return (
            f"SyscallBoundaryError(syscall_nr={self.syscall_nr}, "
            f"instruction_pointer={hex(self.instruction_pointer)})"
        )


class StateDeltaComputationError(DryExecError):
    """Raised when the state delta engine encounters an error inspecting memory or ephemeral filesystem state."""
    pass
