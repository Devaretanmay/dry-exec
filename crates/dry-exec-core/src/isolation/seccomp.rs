//! Seccomp-BPF syscall interception filter builder and runtime attachment.

use crate::error::IsolationError;

pub const AUDIT_ARCH_X86_64: u32 = 0xc000_003e;
pub const AUDIT_ARCH_AARCH64: u32 = 0xc000_00b7;

#[cfg(target_arch = "x86_64")]
pub const CURRENT_AUDIT_ARCH: u32 = AUDIT_ARCH_X86_64;

#[cfg(target_arch = "aarch64")]
pub const CURRENT_AUDIT_ARCH: u32 = AUDIT_ARCH_AARCH64;

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
pub const CURRENT_AUDIT_ARCH: u32 = 0;

/// Action triggered when a syscall matches or defaults within the BPF filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyscallAction {
    Allow,
    Trap,
    KillProcess,
    Errno(u16),
}

impl SyscallAction {
    pub fn to_seccomp_ret(self) -> u32 {
        match self {
            Self::Allow => libc::SECCOMP_RET_ALLOW,
            Self::Trap => libc::SECCOMP_RET_TRAP,
            Self::KillProcess => libc::SECCOMP_RET_KILL_PROCESS,
            Self::Errno(e) => libc::SECCOMP_RET_ERRNO | (e as u32 & 0x0000ffff),
        }
    }
}

/// Seccomp-BPF filter generator enforcing deterministic syscall interception boundaries.
#[derive(Debug, Clone)]
pub struct SeccompFilter {
    default_action: SyscallAction,
    allowed_syscalls: Vec<i64>,
    trapped_syscalls: Vec<i64>,
}

impl Default for SeccompFilter {
    fn default() -> Self {
        Self::new(SyscallAction::Trap)
    }
}

impl SeccompFilter {
    /// Construct a new filter with a designated default boundary action.
    pub fn new(default_action: SyscallAction) -> Self {
        Self {
            default_action,
            allowed_syscalls: Vec::new(),
            trapped_syscalls: Vec::new(),
        }
    }

    /// Whitelist a specific syscall number.
    pub fn allow(mut self, syscall_nr: i64) -> Self {
        if !self.allowed_syscalls.contains(&syscall_nr) {
            self.allowed_syscalls.push(syscall_nr);
        }
        self
    }

    /// Trap a specific syscall while allowing the remainder of the runtime surface.
    pub fn trap(mut self, syscall_nr: i64) -> Self {
        if !self.trapped_syscalls.contains(&syscall_nr) {
            self.trapped_syscalls.push(syscall_nr);
        }
        self
    }

    /// Whitelist standard baseline compute and IO syscalls.
    pub fn with_baseline_whitelist(mut self) -> Self {
        let baseline = [
            libc::SYS_read,
            libc::SYS_write,
            libc::SYS_close,
            libc::SYS_fstat,
            libc::SYS_lseek,
            libc::SYS_mmap,
            libc::SYS_munmap,
            libc::SYS_mprotect,
            libc::SYS_brk,
            libc::SYS_exit,
            libc::SYS_exit_group,
            libc::SYS_rt_sigreturn,
            libc::SYS_sigaltstack,
            libc::SYS_getpid,
            libc::SYS_gettid,
            // Ephemeral filesystem IO primitives for in-boundary state mutation
            libc::SYS_openat,
            libc::SYS_newfstatat,
            libc::SYS_statx,
            libc::SYS_ftruncate,
            libc::SYS_fsync,
            libc::SYS_pread64,
            libc::SYS_pwrite64,
            libc::SYS_readv,
            libc::SYS_writev,
            libc::SYS_getdents64,
            // Memory and runtime support primitives
            libc::SYS_mremap,
            libc::SYS_futex,
            libc::SYS_getrandom,
            libc::SYS_clock_gettime,
            libc::SYS_rt_sigprocmask,
            libc::SYS_sched_yield,
        ];

        for nr in baseline {
            self = self.allow(nr);
        }
        self
    }

    /// Compile the rules into raw classic BPF (cBPF) instructions.
    pub fn compile_bpf(&self) -> Vec<libc::sock_filter> {
        // 1. Validate architecture: Load arch from struct seccomp_data (offset 4)
        // 2. Load syscall number from struct seccomp_data (offset 0)
        let mut filter: Vec<libc::sock_filter> = vec![
            // BPF_LD | BPF_W | BPF_ABS, k = offsetof(struct seccomp_data, arch)
            libc::sock_filter {
                code: (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
                jt: 0,
                jf: 0,
                k: 4,
            },
            // Jump to next instruction if arch matches CURRENT_AUDIT_ARCH, otherwise jump to kill
            libc::sock_filter {
                code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
                jt: 1,
                jf: 0,
                k: CURRENT_AUDIT_ARCH,
            },
            // Kill process on architecture mismatch
            libc::sock_filter {
                code: (libc::BPF_RET | libc::BPF_K) as u16,
                jt: 0,
                jf: 0,
                k: libc::SECCOMP_RET_KILL_PROCESS,
            },
            libc::sock_filter {
                code: (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
                jt: 0,
                jf: 0,
                k: 0,
            },
        ];

        // 3. Trap selected syscalls before evaluating the allow list.
        for &syscall_nr in &self.trapped_syscalls {
            filter.push(libc::sock_filter {
                code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
                jt: 0,
                jf: 0,
                k: syscall_nr as u32,
            });
            filter.push(libc::sock_filter {
                code: (libc::BPF_RET | libc::BPF_K) as u16,
                jt: 0,
                jf: 0,
                k: libc::SECCOMP_RET_TRAP,
            });
        }

        // 4. For each allowed syscall, compare and conditionally jump to ALLOW
        let n_rules = self.allowed_syscalls.len();
        for (i, &syscall_nr) in self.allowed_syscalls.iter().enumerate() {
            let jump_offset_to_allow = (n_rules - 1 - i + 1) as u8;
            filter.push(libc::sock_filter {
                code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
                jt: jump_offset_to_allow,
                jf: 0,
                k: syscall_nr as u32,
            });
        }

        // 5. Fallthrough: Default action (e.g. SECCOMP_RET_TRAP)
        filter.push(libc::sock_filter {
            code: (libc::BPF_RET | libc::BPF_K) as u16,
            jt: 0,
            jf: 0,
            k: self.default_action.to_seccomp_ret(),
        });

        // 6. Whitelisted target: SECCOMP_RET_ALLOW
        filter.push(libc::sock_filter {
            code: (libc::BPF_RET | libc::BPF_K) as u16,
            jt: 0,
            jf: 0,
            k: libc::SECCOMP_RET_ALLOW,
        });

        filter
    }

    /// Attach the compiled filter to the calling thread's kernel boundary.
    pub fn attach(&self) -> Result<(), IsolationError> {
        // Enforce no new privileges prior to attaching filter
        let ret = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
        if ret != 0 {
            return Err(IsolationError::SeccompFailure(format!(
                "Failed to set PR_SET_NO_NEW_PRIVS: errno {}",
                std::io::Error::last_os_error()
            )));
        }

        let bpf_program = self.compile_bpf();
        let prog = libc::sock_fprog {
            len: bpf_program.len() as u16,
            filter: bpf_program.as_ptr() as *mut libc::sock_filter,
        };

        let ret = unsafe {
            libc::syscall(
                libc::SYS_seccomp,
                libc::SECCOMP_SET_MODE_FILTER,
                0,
                &prog as *const libc::sock_fprog,
            )
        };

        if ret != 0 {
            return Err(IsolationError::SeccompFailure(format!(
                "Failed to load seccomp BPF filter program: errno {}",
                std::io::Error::last_os_error()
            )));
        }

        Ok(())
    }
}
