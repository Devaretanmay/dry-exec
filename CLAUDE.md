# AGENT DIRECTIVES: `dry-exec`

## Identity & Purpose
`dry-exec` is an ephemeral execution primitive and deterministic state delta engine for autonomous execution loops. It provides the kernel-level execution layer to explore state mutations without committing changes to durable environments.

## Repository Architecture & Loops
- **Loop 1: Kernel Isolation Boundary** (`crates/dry-exec-core/src/isolation/`):
  - Linux namespaces (`CLONE_NEWPID`, `CLONE_NEWNET`, `CLONE_NEWNS`, `CLONE_NEWIPC`, `CLONE_NEWUTS`).
  - `seccomp-bpf` cBPF filter compiler with `SECCOMP_RET_TRAP` for syscall interception.
  - IPC sync socketpair for child-parent lifecycle coordination.
- **Loop 2: State Delta Engine** (`crates/dry-exec-core/src/delta/`):
  - Copy-on-Write (CoW) memory mapping (`MAP_PRIVATE`).
  - Kernel soft-dirty bitfield tracking via `/proc/[pid]/pagemap` in $O(P_{\text{dirty}})$ time.
  - Ephemeral `tmpfs` inode diffing for filesystem mutations.
- **Loop 3: Type-Safe Python SDK & PyO3 FFI** (`crates/dry-exec-pyo3/`, `python/dry_exec/`):
  - Pydantic V2 synchronous schema validation (`Environment`, `Action`).
  - Non-blocking PyO3 FFI bridge releasing Python GIL via `py.allow_threads`.
  - Structured exception hierarchy (`SyscallBoundaryError`, `SchemaViolationError`).
- **Loop 4: Transparent Network Proxy** (`crates/dry-exec-core/src/delta/network.rs`):
  - Loopback network namespace redirection.
  - Deterministic schema mocking with 500ms timeout bound.
  - `network_mutations` recorded in `StateDelta`.
- **Loop 5: Developer CLI** (`python/dry_exec/cli.py`):
  - Typer-powered commands: `run`, `inspect`, `version`.
  - Human-in-the-loop commit confirmation before durable execution.
- **Loop 6: Observability & Receipts** (`python/dry_exec/observability.py`):
  - Rich-formatted `DeltaLogger` output displaying state delta receipts.
- **Loop 7: Production Examples** (`examples/`):
  - `examples/getting_started/`: Minimal 10-line setup.
  - `examples/use_cases/`: Self-correcting DB migrations and API payment exploration.
  - `examples/integrations/`: LangChain tool wrapper.
- **Loop 8: Production Restructuring & CI/CD** (`.github/`, `docker/`, `scripts/`):
  - GitHub Actions workflows for CI, linting, and releases.
  - Pre-commit configurations and developer shell helpers.

## Linguistic Compliance Directives
All documentation and implementations must maintain engineering precision without anthropomorphic or defensive abstractions:
- Required concepts: deterministic, ephemeral, isolated, state-mutation, boundary, control flow, primitive, execution layer, namespace, syscall interception, state delta, type-safe, transparent proxy, autonomous execution loop.

## Build & Test Instructions
- **Rust Core Check**:
  `cargo check --workspace --tests --target x86_64-unknown-linux-gnu`
- **Unit & Integration Tests**:
  `python3 -m pytest tests/test_examples.py tests/test_cli.py tests/test_sdk.py -v`
- **Containerized Verification Matrix**:
  `./docker/build.sh && docker run --rm --privileged --cap-add=SYS_ADMIN --cap-add=SYS_PTRACE dry-exec-test-harness`
- **Gate Checking**:
  `node ~/.gemini/config/skills/unlazy/scripts/gate-check.mjs GATES.md`
