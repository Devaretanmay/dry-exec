//! Linux namespace isolation primitives.

use bitflags::bitflags;
use nix::sched::{unshare, CloneFlags};
use crate::error::IsolationError;

bitflags! {
    /// Linux namespace isolation boundary flags.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct NamespaceFlags: u32 {
        /// Isolate process IDs; ephemeral child becomes PID 1.
        const PID = libc::CLONE_NEWPID as u32;

        /// Isolate network stack; prevents external network communication.
        const NET = libc::CLONE_NEWNET as u32;

        /// Isolate mount table; changes to mounts do not propagate to host.
        const MOUNT = libc::CLONE_NEWNS as u32;

        /// Isolate System V IPC and POSIX message queues.
        const IPC = libc::CLONE_NEWIPC as u32;

        /// Isolate hostname and domain name identifiers.
        const UTS = libc::CLONE_NEWUTS as u32;
    }
}

impl Default for NamespaceFlags {
    fn default() -> Self {
        Self::PID | Self::NET | Self::MOUNT | Self::IPC | Self::UTS
    }
}

impl NamespaceFlags {
    /// Convert to nix CloneFlags representation.
    pub fn to_clone_flags(self) -> CloneFlags {
        let mut flags = CloneFlags::empty();
        if self.contains(Self::PID) {
            flags |= CloneFlags::CLONE_NEWPID;
        }
        if self.contains(Self::NET) {
            flags |= CloneFlags::CLONE_NEWNET;
        }
        if self.contains(Self::MOUNT) {
            flags |= CloneFlags::CLONE_NEWNS;
        }
        if self.contains(Self::IPC) {
            flags |= CloneFlags::CLONE_NEWIPC;
        }
        if self.contains(Self::UTS) {
            flags |= CloneFlags::CLONE_NEWUTS;
        }
        flags
    }

    /// Disassociate the calling thread's execution layer namespaces.
    pub fn apply_unshare(self) -> Result<(), IsolationError> {
        let flags = self.to_clone_flags();
        unshare(flags).map_err(|e| {
            IsolationError::NamespaceFailure(format!(
                "Failed to unshare execution layer namespaces with flags {flags:?}: {e}"
            ))
        })
    }
}
