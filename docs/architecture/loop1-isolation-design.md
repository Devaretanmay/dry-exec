# Loop 1 Architecture: Kernel Isolation Primitive

## 1. System Overview

`dry-exec` provides an ephemeral execution boundary, enabling deterministic state exploration by isolating state-mutation primitives at the kernel level.

The Loop 1 objective establishes the ephemeral execution boundary in the Rust core control plane (`crates/dry-exec-core`). The primitive clones the execution layer into dedicated Linux namespaces and attaches a deterministic `seccomp-bpf` syscall interception filter before transferring control flow to the target action.

```
+-------------------------------------------------------------+
|               Rust Core Control Plane (Parent)              |
|  - Manages ephemeral lifecycle & boundaries                 |
|  - Spawns isolated execution layer via clone(2)             |
|  - Awaits process termination or syscall interception signal |
+------------------------------+------------------------------+
                               | clone(CLONE_NEWPID | CLONE_NEWNET | CLONE_NEWNS)
                               v
+-------------------------------------------------------------+
|             Ephemeral Execution Layer (Child)               |
|  1. Namespace isolation configuration                       |
|     - Private mount propagation & sterile /proc             |
|     - Loopback-only isolated network namespace              |
|     - In-namespace transparent proxy mock listener          |
|  2. Seccomp-BPF syscall interception filter attachment      |
|     - Strict whitelist of allowed syscalls                  |
|     - Immediate SIGSYS termination on boundary breach       |
|  3. Action invocation & state-mutation execution            |
+-------------------------------------------------------------+
```

---

## 2. Module Hierarchy & Crate Layout

```
crates/dry-exec-core/
├── Cargo.toml
└── src/
    ├── lib.rs                  # Crate root and high-level boundary interface
    ├── error.rs                # Type-safe kernel boundary error definitions
    └── isolation/
        ├── mod.rs              # Isolation primitive entry point and lifecycle supervisor
        ├── namespace.rs        # Linux namespace cloning and unshare primitives
        ├── mount.rs            # Mount namespace isolation, tmpfs root, private propagation
        ├── seccomp.rs          # Seccomp-BPF filter builder and syscall interception table
        └── process.rs          # Process clone execution and status reporting
```

---

## 3. Structural Design & Type-Safe Interfaces

### 3.1 Namespace Isolation Specification (`isolation::namespace`)

Namespace isolation segregates the kernel resources of the execution layer from the host environment:

*   **`CLONE_NEWPID`**: Isolates process IDs. The ephemeral child becomes PID 1 inside its namespace.
*   **`CLONE_NEWNET`**: Eliminates external socket communication. Only a localized loopback interface is instantiated.
*   **`CLONE_NEWNS`**: Isolates mount points, preventing external filesystem state mutations.
*   **`CLONE_NEWIPC`**: Prevents System V IPC and POSIX message queue boundary crossing.
*   **`CLONE_NEWUTS`**: Isolates hostname and NIS domain identifiers.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NamespaceFlags {
    pub pid: bool,
    pub net: bool,
    pub mount: bool,
    pub ipc: bool,
    pub uts: bool,
}

impl Default for NamespaceFlags {
    fn default() -> Self {
        Self {
            pid: true,
            net: true,
            mount: true,
            ipc: true,
            uts: true,
        }
    }
}
```

### 3.2 Syscall Interception Filter Specification (`isolation::seccomp`)

The syscall interception engine compiles a BPF (Berkeley Packet Filter) program attached via `prctl(PR_SET_NO_NEW_PRIVS, 1, ...)` and `seccomp(SECCOMP_SET_MODE_FILTER, ...)`.

*   **Default Action**: `SECCOMP_RET_KILL_PROCESS` (or `SECCOMP_RET_TRAP` for verification inspection).
*   **Permitted Actions**: `SECCOMP_RET_ALLOW` strictly mapped to an immutable whitelist of syscall numbers.
*   **Syscall Whitelist**:
    *   State inspection / standard I/O: `read`, `write`, `close`, `fstat`, `lseek`.
    *   Memory management: `mmap` (read/write/private), `munmap`, `mprotect`, `brk`.
    *   Control flow termination: `exit`, `exit_group`.
    *   Blocked by default: `socket`, `connect`, `bind`, `execve`, `fork`, `clone`, `kill`.

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyscallAction {
    Allow,
    Trap,
    KillProcess,
    Errno(u16),
}

#[derive(Debug, Clone)]
pub struct SyscallRule {
    pub syscall_nr: i64,
    pub action: SyscallAction,
}

pub struct SeccompFilter {
    default_action: SyscallAction,
    rules: Vec<SyscallRule>,
}
```

### 3.3 Ephemeral Process Control Plane (`isolation::process`)

The parent process initializes a synchronization channel (Unix domain socketpair or pipe with `O_CLOEXEC`), clones into the execution layer, applies the seccomp-bpf filter inside the child, executes the closure/payload, and reports the termination status back across the boundary.

```rust
#[derive(Debug)]
pub enum BoundaryExitStatus {
    Exited(i32),
    Signaled(i32),
    SyscallViolation { syscall_nr: u32, ip: u64 },
}

pub struct IsolatedProcess {
    child_pid: nix::unistd::Pid,
    sync_fd: std::os::unix::io::RawFd,
}
```

### 3.4 Transparent Proxy Boundary Handshake (`delta::network`)

The isolated execution layer occupies a dedicated `CLONE_NEWNET`, so a listener bound on the control plane's loopback is unreachable from within the boundary. The mock listener must therefore be bound **inside** the isolated namespace, while the requests it intercepts are recorded on the control plane.

1.  The control plane reserves a loopback port (`TransparentProxy::reserve_loopback_port`) and carries the schema-driven route table in the boundary configuration.
2.  The execution layer enables the namespace loopback interface (`SIOCSIFFLAGS`), which starts `down` and yields `ENETUNREACH` on connect until enabled.
3.  The execution layer binds the schema listener on that port and forwards the descriptor to the control plane as `SCM_RIGHTS` control data attached to the readiness frame.
4.  The control plane constructs `TransparentProxy::from_listener` over the forwarded descriptor and serves it: the socket stays bound inside the isolated namespace, so requests originating there reach the proxy, while accepted connections and the mutation ledger remain on the control plane.
5.  During inspection the proxy is surfaced through `BoundaryContext::network` and adopted by `DeltaCoordinator::set_network_proxy`, so intercepted requests are aggregated into `StateDelta.network_mutations` alongside memory and filesystem mutations.

---

## 4. Control Flow Progression

1.  **Preparation Phase**: Control plane establishes synchronization pipe and precomputes BPF bytecode instructions.
2.  **Clone Phase**: `clone(2)` or `clone3` executes with `SIGCHLD | CLONE_NEWPID | CLONE_NEWNET | CLONE_NEWNS | CLONE_NEWIPC | CLONE_NEWUTS`.
3.  **Child Initialization Phase**:
    *   Set private mount propagation: `mount(NULL, "/", NULL, MS_REC | MS_PRIVATE, NULL)`.
    *   Mount isolated ephemeral `/proc` and sterile tmpfs overlays.
    *   Set `PR_SET_NO_NEW_PRIVS`.
    *   Load BPF filter into the kernel via `seccomp(SECCOMP_SET_MODE_FILTER, 0, &fprog)`.
    *   Signal parent readiness across the synchronization channel.
4.  **Execution Phase**: Execute target state-mutation action within the isolated boundary.
5.  **Interception / Termination Phase**:
    *   If a blocked syscall is executed: Kernel intercepts the syscall and raises `SIGSYS` / kills the isolated process immediately.
    *   If execution completes: Return state mutation exit code and close descriptors.
6.  **Reaping & Inspection Phase**: Parent reaps child using `waitpid(__WALL)`, decodes termination signal or exit status, and confirms deterministic boundary preservation.
