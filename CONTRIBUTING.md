# Contributing to `dry-exec`

Thank you for your interest in contributing to `dry-exec`. We welcome contributions to our ephemeral execution primitive and deterministic state delta engine.

## Architectural Mental Model

`dry-exec` is a kernel-level execution layer designed for autonomous execution loops. It guarantees deterministic state-mutation tracking and ephemeral boundaries via:
1. **Linux Namespace Isolation**: Strict separation across PID, NET, MOUNT, IPC, and UTS namespaces.
2. **Deterministic State Deltas**: $O(P_{\text{dirty}})$ memory page-table diffing via kernel soft-dirty tracking and tmpfs inode diffs.
3. **Transparent Proxy Interception**: Interception of external API endpoints with deterministic schema mocking.
4. **Type-Safe Python SDK & PyO3 FFI**: Synchronous Pydantic V2 validation and non-blocking runtime execution.

## Architectural Terminology

To preserve engineering precision and avoid ambiguous abstractions, all code, documentation, pull requests, and commit messages must adhere to kernel and systems terminology:
- Focus on: `deterministic`, `ephemeral`, `isolated`, `state-mutation`, `boundary`, `control flow`, `primitive`, `execution layer`, `namespace`, `syscall interception`, `state delta`, `type-safe`, `transparent proxy`, `autonomous execution loop`.

## Development Setup

### Prerequisites
- **Linux Kernel**: Linux 5.x+ with support for namespaces and soft-dirty pagemap (`/proc/[pid]/pagemap`). On macOS/Windows, development requires Docker.
- **Rust Toolchain**: 1.80+ (`cargo`, `rustfmt`, `clippy`).
- **Python**: 3.10+ with `venv`, `pip`, and `maturin`.
- **Docker**: For running the containerized verification harness.

### Local Workflow
1. Clone the repository:
   ```bash
   git clone https://github.com/dry-exec/dry-exec.git
   cd dry-exec
   ```

2. Run developer setup and linting:
   ```bash
   ./scripts/lint.sh
   ```

3. Build the core and Python extension:
   ```bash
   ./scripts/build.sh
   ```

4. Run the verification test suite:
   ```bash
   ./scripts/test.sh
   ```

## Pull Request Guidelines

1. **Deterministic Verification**: Every pull request must pass the automated CI workflows (`ci.yml`, `lint.yml`).
2. **Zero In-Memory State Leaks**: All test cases mutating memory or disk must verify complete teardown after execution.
3. **Precision Compliance**: Avoid vague non-technical wrappers; adhere strictly to kernel isolation and type-safe invariants.
4. **Conventional Commits**: Format commit messages cleanly (e.g., `feat: implement seccomp trap handler`, `refactor: simplify pagemap byte decoding`).
