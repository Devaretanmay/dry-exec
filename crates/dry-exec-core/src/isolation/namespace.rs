//! Linux namespace isolation primitives.

use crate::error::IsolationError;
use bitflags::bitflags;
use nix::sched::{unshare, CloneFlags};
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};

/// `struct ifreq` is 40 bytes on 64-bit Linux: a 16-byte interface name plus a 24-byte union.
#[repr(C)]
struct IfReqFlags {
    ifr_name: [libc::c_char; libc::IFNAMSIZ],
    ifr_flags: libc::c_short,
    _reserved: [u8; 22],
}

/// Bring the loopback interface of the calling network namespace up.
///
/// An isolated network namespace starts with `lo` down, so a bound loopback listener is
/// unreachable (`ENETUNREACH`) until the interface is enabled. The isolated execution layer
/// performs this wiring so the transparent proxy is reachable from within the boundary.
pub fn bring_loopback_up() -> Result<(), IsolationError> {
    const IFF_UP: libc::c_short = 0x1;

    let raw_fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    if raw_fd < 0 {
        return Err(IsolationError::NamespaceFailure(format!(
            "Failed to open interface control socket: {}",
            std::io::Error::last_os_error()
        )));
    }
    let sock = unsafe { OwnedFd::from_raw_fd(raw_fd) };

    let mut request = IfReqFlags {
        ifr_name: [0; libc::IFNAMSIZ],
        ifr_flags: 0,
        _reserved: [0; 22],
    };
    for (slot, byte) in request.ifr_name.iter_mut().zip(b"lo\0") {
        *slot = *byte as libc::c_char;
    }

    if unsafe { libc::ioctl(sock.as_raw_fd(), libc::SIOCGIFFLAGS, &mut request) } < 0 {
        return Err(IsolationError::NamespaceFailure(format!(
            "Failed to read loopback interface flags: {}",
            std::io::Error::last_os_error()
        )));
    }

    if request.ifr_flags & IFF_UP != 0 {
        return Ok(());
    }

    request.ifr_flags |= IFF_UP;
    if unsafe { libc::ioctl(sock.as_raw_fd(), libc::SIOCSIFFLAGS, &request) } < 0 {
        return Err(IsolationError::NamespaceFailure(format!(
            "Failed to enable loopback interface: {}",
            std::io::Error::last_os_error()
        )));
    }

    Ok(())
}

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
