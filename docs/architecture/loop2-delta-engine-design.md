# Loop 2 Architecture: State Delta Engine

## 1. System Overview

`dry-exec` provides an ephemeral execution boundary, enabling deterministic state exploration by isolating state-mutation primitives at the kernel level.

The Loop 2 objective establishes the **State Delta Engine** within the Rust core control plane (`crates/dry-exec-core/src/delta`). Following isolated execution in the Loop 1 kernel boundary, the State Delta Engine computes the exact state mutation across memory and ephemeral filesystem layers without full state duplication or $O(N)$ application-level hashing.

```
+-------------------------------------------------------------------------+
|                    Rust Core Control Plane (Parent)                     |
|                                                                         |
|  1. Pre-Execution Setup:                                                |
|     - Map initial state via mmap(MAP_PRIVATE)                           |
|     - Reset dirty page bits via /proc/[pid]/clear_refs                  |
|     - Snapshot tmpfs inode metadata directory tree                      |
|                                                                         |
|  2. Ephemeral Isolation (Loop 1 Primitive):                             |
|     - Fork/clone into isolated namespaces (PID, NET, MOUNT, IPC)        |
|     - Attach SECCOMP_RET_TRAP syscall interception boundary             |
|                                                                         |
|  3. Execution Layer State Mutation (Child):                             |
|     - Executes isolated state-mutating action                           |
|     - Kernel CoW duplicates only written memory pages                   |
|     - Kernel marks dirty bit in page table for modified pages           |
|     - Mutations written to isolated tmpfs overlay                       |
|                                                                         |
|  4. Post-Execution State Delta Computation:                             |
|     - Inspect /proc/[pid]/pagemap bit 55 (soft-dirty flag)              |
|     - Compute byte deltas strictly for dirty pages: O(P_dirty)          |
|     - Diff tmpfs inode metadata tree against pre-execution snapshot     |
|     - Construct structured, type-safe StateDelta                        |
+-------------------------------------------------------------------------+
```

---

## 2. Module Hierarchy & Crate Layout

```
crates/dry-exec-core/
├── Cargo.toml
└── src/
    ├── lib.rs                  # Root crate interface
    ├── error.rs                # Type-safe kernel boundary error definitions
    ├── isolation/              # Loop 1: Namespace & syscall interception boundary
    │   ├── mod.rs
    │   ├── namespace.rs
    │   ├── mount.rs
    │   ├── seccomp.rs
    │   └── process.rs
    └── delta/                  # Loop 2: State Delta Engine
        ├── mod.rs              # Delta engine coordinator & StateDelta definition
        ├── memory.rs           # CoW memory mapping & page-table dirty tracking
        ├── pagemap.rs          # /proc/[pid]/pagemap & soft-dirty bit reader
        ├── fs.rs               # Ephemeral tmpfs inode diffing engine
        └── types.rs            # Type-safe delta representations (ByteDelta, InodeDelta)
```

---

## 3. Structural Design & Kernel Primitives

### 3.1 Copy-on-Write (CoW) Memory Isolation (`delta::memory`)

Memory state exploration requires zero-cost duplication of base state:

*   **Mapping:** The target state is memory-mapped using `mmap(NULL, len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0)` or backed by a file descriptor.
*   **Kernel CoW Behavior:** Pre-fork, physical memory pages are shared between the control plane and the child execution layer. When the isolated child writes to an address, the kernel page-fault handler intercepts the write, allocates a new physical frame, copies the 4KB page, and updates the child's page table.
*   **Boundary Guarantee:** The control plane's original memory mapping remains completely untouched by child mutations.

### 3.2 Page-Table Dirty Tracking (`delta::pagemap`)

Application-level hashing of large state regions (e.g., computing SHA-256 over a 10MB–1GB state) incurs unacceptable $O(N)$ execution latency. The State Delta Engine implements kernel-level page-table tracking using Linux soft-dirty bits (`CONFIG_MEM_SOFT_DIRTY`):

1.  **Bit Clearing (Pre-Execution):** The control plane clears soft-dirty tracking by writing `"4\n"` to `/proc/self/clear_refs` (or `/proc/[pid]/clear_refs`).
2.  **Child Execution:** As the isolated action mutates memory, the kernel hardware page table sets the dirty flag, transitioning the virtual page to soft-dirty.
3.  **Bit Reading (Post-Execution):** The State Delta Engine reads `/proc/[pid]/pagemap`. Each 4KB virtual page has a corresponding 64-bit descriptor:
    *   **Bit 55:** `PAGE_SOFT_DIRTY` flag (1 if written since `clear_refs`, 0 otherwise).
    *   **Bit 63:** `PAGE_PRESENT` flag.
4.  **$O(P_{\text{dirty}})$ Delta Scan:**
    *   For a 10MB memory mapping ($2,560$ pages of 4KB), `/proc/[pid]/pagemap` consumes exactly $20,480$ bytes. Reading and scanning $2,560$ 64-bit words takes under $10\,\mu\text{s}$.
    *   Only pages with Bit 55 set are read and compared against the baseline mapping.
    *   Zero false positives: clean pages are skipped entirely.

### 3.3 Ephemeral Filesystem Inode Diffing (`delta::fs`)

Filesystem mutations are confined to an isolated `tmpfs` mount created in the mount namespace:

1.  **Pre-Execution Inode Snapshot:** Traverse the directory tree of the ephemeral mount point, storing a table of `InodeRecord` entries:
    ```rust
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct InodeRecord {
        pub ino: u64,
        pub path: std::path::PathBuf,
        pub size: u64,
        pub mode: u32,
        pub mtime_nsec: i64,
        pub ctime_nsec: i64,
    }
    ```
2.  **Post-Execution Traversal & Classification:**
    *   **Added Inodes:** Inodes or paths present post-execution but absent pre-execution.
    *   **Deleted Inodes:** Paths present pre-execution but unlinked during execution.
    *   **Mutated Inodes:** Matching paths where `size`, `mtime`, or content altered.
3.  **Content Delta Extraction:** For mutated files, compute exact byte offsets of file modifications within the `tmpfs` overlay.

---

## 4. Type-Safe Rust Interfaces

```rust
use std::path::PathBuf;

/// Exact byte mutation within a memory page or file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ByteDelta {
    pub offset: usize,
    pub original: Vec<u8>,
    pub mutated: Vec<u8>,
}

/// State mutation detected on a single virtual memory page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageMutation {
    pub page_index: usize,
    pub page_address: usize,
    pub deltas: Vec<ByteDelta>,
}

/// Filesystem mutation records for the ephemeral tmpfs boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsMutation {
    Created { path: PathBuf, mode: u32, size: u64 },
    Modified { path: PathBuf, deltas: Vec<ByteDelta> },
    Deleted { path: PathBuf },
}

/// Complete deterministic state delta returned to the control flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateDelta {
    pub memory_mutations: Vec<PageMutation>,
    pub fs_mutations: Vec<FsMutation>,
    pub total_bytes_mutated: usize,
    pub duration_nanos: u64,
}

pub trait StateDeltaEngine {
    /// Initialize CoW tracking on the designated memory region.
    fn prepare_memory_region(&mut self, addr: *mut u8, len: usize) -> Result<(), crate::error::DeltaError>;

    /// Snapshot baseline filesystem state on ephemeral tmpfs root.
    fn snapshot_fs(&mut self, root: &std::path::Path) -> Result<(), crate::error::DeltaError>;

    /// Compute exact state mutation delta post-execution.
    fn compute_delta(&self, pid: nix::unistd::Pid) -> Result<StateDelta, crate::error::DeltaError>;
}
```

---

## 5. Verification Parameters & Performance Assertions

1.  **State Delta Precision (Zero False Positives):**
    *   Initialize a 10MB memory-mapped buffer.
    *   Execute an isolated state-mutating action writing exactly 4 bytes (`0xDEADBEEF`) at offset `0x004A_0000` (page 1184) and creating `/ephemeral/mutated.bin`.
    *   The engine must return `memory_mutations.len() == 1`, with exact offset `0x004A_0000`, `mutated == [0xDE, 0xAD, 0xBE, 0xEF]`, and `fs_mutations.len() == 1`.
2.  **Bounded Computational Complexity ($O(1)$ relative to total state size):**
    *   Diff computation time must depend strictly on the number of mutated pages ($P_{\text{dirty}}$), not the total buffer size $N$.
    *   Comparing a 4-byte mutation in a 10MB region versus a 100MB region must yield equivalent diffing latency within kernel pagemap scan tolerances.
3.  **Non-Linux Target Enforcement:**
    *   `crates/dry-exec-core` will enforce compile-time gating:
        ```rust
        #[cfg(not(target_os = "linux"))]
        compile_error!("dry-exec requires Linux kernel primitives (namespaces, seccomp-bpf, soft-dirty pagemap). Build and execute tests inside the provided Linux container verification harness.");
        ```
