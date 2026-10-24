# Contributing to dry-exec

Thanks for your interest in contributing to `dry-exec`! We love contributions that make our ephemeral execution primitive and deterministic state delta engine faster, safer, and simpler to run.

## Architectural mental model

`dry-exec` is a kernel-level execution layer designed for autonomous AI agents. It gives each proposed action an isolated trial and returns a structured `StateDelta` receipt before anything is committed to a durable environment:
1. **Linux namespace isolation**: Strict separation across PID, NET, MOUNT, IPC, and UTS namespaces.
2. **macOS Seatbelt sandboxing**: Kernel-level mandatory access control (MAC) combined with APFS copy-on-write `clonefile` snapshots.
3. **Deterministic state deltas**: $O(P_{\text{dirty}})$ memory page-table diffing via kernel soft-dirty tracking and tmpfs inode diffs.
4. **Transparent proxy interception**: Ephemeral loopback proxy that serves deterministic mock responses for registered routes and records schema breaches for unregistered routes.
5. **Type-safe Python SDK and PyO3 FFI**: Synchronous Pydantic V2 validation, release of the Python GIL during kernel isolation, and non-blocking runtime execution.

## Architectural terminology

To keep engineering precision high and avoid ambiguous abstractions, all code, documentation, pull requests, and commit messages should stick to systems and kernel terminology:
- Recommended terms: `deterministic`, `ephemeral`, `isolated`, `state-mutation`, `boundary`, `control flow`, `primitive`, `execution layer`, `namespace`, `syscall interception`, `state delta`, `type-safe`, `transparent proxy`, and `autonomous execution loop`.

## Development setup

### Prerequisites
- **Operating system**:
  - **macOS**: 11.0+ with Apple Silicon or Intel (runs natively with Seatbelt and APFS).
  - **Linux**: Landlock ABI 3 or newer, namespaces, and soft-dirty pagemap (`/proc/[pid]/pagemap`).
- **Rust toolchain**: 1.85+ (`cargo`, `rustfmt`, and `clippy`), as required by the locked dependencies.
- **Python**: 3.10+ with `venv`, `pip`, and `maturin`.
- **Docker** (optional): For running the Linux containerized verification matrix across platforms.

### Local workflow

1. Clone the repository:
   ```bash
   git clone https://github.com/Devaretanmay/dry-exec.git
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

5. (Optional) Run the complete Linux container verification suite:
   ```bash
   ./docker/build.sh
   docker run --rm --privileged --cap-add=SYS_ADMIN --cap-add=SYS_PTRACE dry-exec-test-harness
   ```

## Pull request guidelines

1. **Deterministic verification**: Every pull request must pass the automated CI workflows (`ci.yml`, `lint.yml`).
2. **Zero in-memory state leaks**: All test cases mutating memory or disk must verify complete teardown after execution.
3. **Precision compliance**: Avoid vague non-technical abstractions; adhere strictly to kernel isolation and type-safe invariants.
4. **Conventional commits**: Format commit messages cleanly (e.g., `feat: implement seccomp trap handler`, `refactor: simplify pagemap byte decoding`).
