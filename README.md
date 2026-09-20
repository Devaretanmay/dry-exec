# dry-exec

`dry-exec` provides an ephemeral execution boundary, enabling deterministic state exploration by isolating state-mutation primitives at the kernel level.

---

## 1. System Overview & Architectural Mental Model

`dry-exec` is a runtime primitive designed for autonomous execution loops. Rather than permitting uncontrolled side-effects or relying on speculative application-level rollback mechanisms, `dry-exec`:
1. Intercepts state-mutating actions before execution.
2. Clones the target execution layer into ephemeral Linux namespaces at the kernel level.
3. Attaches deterministic `seccomp-bpf` syscall interception filters.
4. Spins up an isolated transparent proxy to intercept outbound network mutations with schema-driven mock responses.
5. Computes the exact state delta ($O(P_{\text{dirty}})$ memory page mutations via kernel soft-dirty bit tracking, filesystem inode diffs, and intercepted network requests).
6. Returns deterministic telemetry to the autonomous execution loop without mutating the baseline state.

```
+-------------------------------------------------------------------------------+
|                    Autonomous Execution Loop (Python SDK)                     |
|                                                                               |
|  - Pre-Execution Boundary: Synchronous Pydantic V2 schema validation          |
|  - Async non-blocking FFI dispatch (releases Python GIL via py.allow_threads) |
|  - Structured exception handling exposing syscall_nr and instruction_pointer  |
+---------------------------------------+---------------------------------------+
                                        |
                                        | PyO3 FFI Boundary (_dry_exec_ffi)
                                        v
+-------------------------------------------------------------------------------+
|                      Rust Core Control Plane (Parent Process)                 |
|                                                                               |
|  - Manages ephemeral process lifecycle and IPC synchronization boundaries     |
|  - Allocates Copy-on-Write memory mapping (mmap MAP_PRIVATE)                  |
|  - Establishes baseline page-table tracking via /proc/[pid]/clear_refs        |
|  - Coordinates local transparent proxy for schema-driven mock interception   |
+---------------------------------------+---------------------------------------+
                                        |
                                        | clone(CLONE_NEWPID | CLONE_NEWNET | CLONE_NEWNS | ...)
                                        v
+-------------------------------------------------------------------------------+
|                      Ephemeral Execution Layer (Child Data Plane)             |
|                                                                               |
|  1. Namespace Boundary:                                                       |
|     - Private mount propagation & sterile tmpfs / /proc                       |
|     - Isolated loopback networking (zero external packet egress)              |
|  2. Syscall Interception Boundary:                                            |
|     - PR_SET_NO_NEW_PRIVS + Seccomp-BPF filter (whitelisted compute/IO)       |
|     - Default SECCOMP_RET_TRAP: dispatches SIGSYS on boundary breach          |
|  3. Isolated State-Mutation Execution:                                        |
|     - Hardware CoW duplicates only modified 4KB physical pages                |
|     - Outbound HTTP requests handled deterministically by local proxy         |
+-------------------------------------------------------------------------------+
```

---

## 2. Directory & Module Hierarchy

```
dry-exec/
├── Cargo.toml                                # Root Cargo workspace configuration
├── pyproject.toml                            # Python package build configuration (Maturin)
├── GATES.md                                  # Verifiable acceptance ledger (32/32 met)
├── CONTRIBUTING.md                           # Community contribution guidelines & terminology
├── SECURITY.md                               # Kernel isolation security & vulnerability model
├── CLAUDE.md                                 # Directives for autonomous execution loops
├── .pre-commit-config.yaml                   # Formatting & linting git hooks
├── .env.example                              # Developer configuration template
├── .github/
│   ├── ISSUE_TEMPLATE/                       # Bug report & feature request templates
│   └── workflows/                            # Pinned-SHA CI, Lint, & Release pipelines
├── docker/
│   ├── Dockerfile.ci                         # Containerized Linux verification runtime
│   └── build.sh                              # Verification container build helper
├── scripts/
│   ├── build.sh                              # Unified workspace build script
│   ├── lint.sh                               # Cargo fmt, check, & linguistic audit script
│   └── test.sh                               # Test harness & gate check script
├── crates/
│   ├── dry-exec-core/                        # Rust Core Control Plane & Data Plane
│   │   ├── Cargo.toml
│   │   ├── src/
│   │   │   ├── lib.rs                        # Target guards & re-exports
│   │   │   ├── error.rs                      # Boundary & delta error types
│   │   │   ├── isolation/                    # Loop 1: Kernel Isolation Boundary
│   │   │   │   ├── namespace.rs              # Linux namespace isolation primitives
│   │   │   │   ├── mount.rs                  # Mount namespace, private propagation, tmpfs
│   │   │   │   ├── seccomp.rs                # Classic BPF (cBPF) filter builder & attachment
│   │   │   │   └── process.rs                # Clone supervisor, IPC sync, SIGSYS interception
│   │   │   └── delta/                        # Loop 2 & Loop 4: State Delta Engine
│   │   │       ├── types.rs                  # StateDelta, PageMutation, InterceptedRequest
│   │   │       ├── memory.rs                 # CoW memory mappings & process_vm_readv access
│   │   │       ├── pagemap.rs                # /proc/[pid]/pagemap soft-dirty bit parser (O(P_dirty))
│   │   │       ├── fs.rs                     # Ephemeral tmpfs inode diffing engine
│   │   │       └── network.rs                # Transparent proxy with schema-driven mocking
│   │   └── tests/
│   │       └── loop1_loop2_tests.rs          # Integration tests for Assertions A, B, and C
│   └── dry-exec-pyo3/                        # Loop 3: High-Performance PyO3 FFI Bridge
│       ├── Cargo.toml
│       └── src/
│           └── lib.rs                        # PyO3 module, GIL release, structured exceptions
├── python/
│   └── dry_exec/                             # Type-Safe Python SDK
│       ├── __init__.py                       # Package exports
│       ├── schemas.py                        # Pydantic V2 Action, Environment, MockResponse
│       ├── models.py                         # StateDelta, PageMutation, InterceptedRequest
│       ├── exceptions.py                     # SchemaViolationError, SyscallBoundaryError, etc.
│       ├── observability.py                  # Rich DeltaLogger receipts
│       ├── cli.py                            # Typer developer CLI
│       └── client.py                         # Async-first DryExecClient with synchronous gatekeeping
├── examples/
│   ├── getting_started/                      # Basic quickstart usage
│   │   └── quickstart.py
│   ├── use_cases/                            # Domain workflows
│   │   ├── type_safe_db_migration.py         # Self-correcting DB schema migration
│   │   └── api_payment_exploration.py        # Mocked payment API interception
│   └── integrations/                         # Agent framework integrations
│       └── langchain_tool.py                 # LangChain tool wrapper
└── tests/
    ├── test_sdk.py                           # Python SDK integration test suite
    ├── test_cli.py                           # CLI & observability verification suite
    ├── test_examples.py                      # Verification across all example workflows
    └── container/                            # Linux Container Verification Harness
        ├── Dockerfile                        # Containerized Linux runtime environment
        ├── run_tests.sh                      # Host execution script
        └── run_suite_inside_container.sh     # Container-internal test runner script
```

---

## 3. Quickstart

```python
import asyncio
from dry_exec import Action, DryExecClient, Environment

async def main():
    # 1. Define isolated execution boundary
    env = Environment(
        name="quickstart_env",
        allowed_mutation_targets={"system_status"},
        memory_limit_bytes=4096,
    )

    # 2. Define proposed mutation action
    action = Action(
        action_id="act_001",
        target_resource="system_status",
        mutation_type="update",
        payload={"status": "online"},
    )

    # 3. Dry-run exploration: inspect exact StateDelta before committing
    client = DryExecClient()
    delta = await client.execute_ephemeral_action(env, action)
    print(f"StateDelta: {delta.total_bytes_mutated} bytes mutated in {delta.duration_nanos} ns.")

if __name__ == "__main__":
    asyncio.run(main())
```

---

## 4. Engineering Milestones (Build Loops)

### Loop 1: The Kernel Isolation Primitive
*   **Process Boundary**: Clones execution into isolated `PID`, `NET`, `MOUNT`, `IPC`, and `UTS` namespaces.
*   **Syscall Interception**: Compiles classic BPF filter validating architecture (`AUDIT_ARCH_X86_64` / `AUDIT_ARCH_AARCH64`), whitelisting minimal compute/IO (`read`, `write`, `fstat`, `lseek`, `mmap`, `munmap`, `exit_group`), and enforcing `SECCOMP_RET_TRAP` as default action.
*   **Deterministic Signal Interception**: Captures `SIGSYS`, serializes `siginfo_t` (`si_syscall`, `si_call_addr`), and returns `BoundaryExitStatus::SyscallViolation`.

### Loop 2: The State Delta Engine
*   **Hardware CoW**: Memory mapped with `MAP_PRIVATE | MAP_ANONYMOUS`; parent and child share physical frames until mutated.
*   **Page-Table Dirty Tracking ($O(P_{\text{dirty}})$)**: Clears soft-dirty bits post-fork via `/proc/[pid]/clear_refs`, parses `/proc/[pid]/pagemap` little-endian descriptors (Bit 63 `PAGE_PRESENT`, Bit 55 `PAGE_SOFT_DIRTY`), and extracts sub-page byte mutations directly against the parent baseline slice without application-level hashing.
*   **Ephemeral Filesystem Diffing**: Snapshots tmpfs directory tree inodes with low-level `lstat`, classifying mutations as `Created`, `Modified`, or `Deleted`.

### Loop 3: The Type-Safe Python SDK & FFI Bridge
*   **Synchronous Gatekeeping**: Pydantic V2 `Environment.validate_action(action)` enforces strict schema matching before FFI invocation, raising `SchemaViolationError` synchronously on unauthorized mutations.
*   **Non-Blocking Async Dispatch**: Rust kernel operations execute with Python GIL released (`py.allow_threads`), preserving `asyncio` event loop responsiveness.
*   **Structured Error Telemetry**: Propagates `SyscallBoundaryError` exposing `syscall_nr` and `instruction_pointer` attributes for control flow self-correction.

### Loop 4: Network & API Interception (The Proxy)
*   **Namespace Boundary**: Outbound requests remain confined within `CLONE_NEWNET`; loopback interface handles local communication.
*   **Transparent Proxy**: Binds local proxy listener, validates HTTP requests against `allowed_api_endpoints`, and delivers deterministic `MockResponse` payloads with a 500ms timeout bound.
*   **Network Delta Ledger**: Extends `StateDelta` with `network_mutations`, capturing request method, URL, headers, outbound payload, status code, and mock response body.

### Loop 5: The Developer CLI & Agent Runner
*   **Command Line Interface**: Provides `dry-exec run`, `dry-exec inspect`, and `dry-exec version`.
*   **Human-In-The-Loop Control Flow**: Inspects proposed action, executes in isolation, renders delta receipt, and awaits user confirmation before state commitment.

### Loop 6: Observability & Delta Visualization
*   **DeltaLogger**: Formats state mutations using `rich` terminal panels, clearly segmenting:
    1. Proposed Action (what the execution loop attempted).
    2. Memory/Filesystem Mutations (exact byte/file mutations).
    3. Network Interceptions (outbound requests caught by proxy and mock responses).
    4. Boundary Violations (`SyscallBoundaryError` and `SchemaViolationError`).

### Loop 7: Production-Ready Example Workflows
*   [`examples/getting_started/quickstart.py`](examples/getting_started/quickstart.py): Minimal 10-line setup.
*   [`examples/use_cases/type_safe_db_migration.py`](examples/use_cases/type_safe_db_migration.py): Self-correcting database migration workflow recovering from schema boundary rejections.
*   [`examples/use_cases/api_payment_exploration.py`](examples/use_cases/api_payment_exploration.py): External API mutation exploration intercepted by the transparent network proxy with zero network egress.
*   [`examples/integrations/langchain_tool.py`](examples/integrations/langchain_tool.py): Autonomous execution loop wrapper for LangChain.

### Loop 8: Production Restructuring & CI/CD
*   GitHub Actions CI/CD with pinned SHAs (`ci.yml`, `lint.yml`, `release.yml`).
*   Automated pre-commit hooks (`.pre-commit-config.yaml`) running `ruff`, `black`, `cargo fmt`, and `cargo clippy`.
*   Developer scripts (`scripts/lint.sh`, `scripts/build.sh`, `scripts/test.sh`) and container automation (`docker/Dockerfile.ci`, `docker/build.sh`).
*   Formal community governance and disclosure specifications (`CONTRIBUTING.md`, `SECURITY.md`, `CLAUDE.md`).

---

## 5. Verification Harness Execution

`dry-exec` enforces compile-time target checking: compilation on non-Linux hosts halts immediately with a directive to use the container harness.

### Running the Containerized Verification Harness
```bash
./scripts/test.sh
```

Or via Docker directly:
```bash
./docker/build.sh
docker run --rm --privileged --cap-add=SYS_ADMIN --cap-add=SYS_PTRACE dry-exec-test-harness
```


