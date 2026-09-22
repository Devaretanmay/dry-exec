# Trust Center

`dry-exec` is an execution boundary for exploring proposed agent actions. It reduces blast radius; it does not make arbitrary code trustworthy or replace production change control.

## Scope and assumptions

- Treat action payloads and command output as untrusted.
- Review returned `StateDelta` before committing durable changes.
- Run Linux kernel tests with the documented privileged harness.
- Keep secrets outside trial inputs and environment variables unless a specific test requires them.

## Threat model

### Prompt injection

Prompt injection can cause an agent to propose an unintended command, target, or endpoint. dry-exec validates the proposed action against the configured target and endpoint schema, then reports the resulting delta. It does not determine whether the agent’s intent is legitimate. Human or application-level approval remains required for durable commit.

### Secret access

The boundary limits filesystem and network access, including explicit secret-path denials in the macOS profile. This is not a guarantee that every application-specific secret location is known. Do not mount secret stores into a trial unless required and reviewed.

### Escape vectors

Relevant escape classes include kernel vulnerabilities, namespace configuration errors, Seatbelt profile errors, unsafe native FFI, device access, and dependency compromise. The project reduces exposure with Linux namespaces, seccomp, macOS Seatbelt, isolated filesystem handling, and focused regression tests. Keep host kernels, Python, Rust, and dependencies patched.

### Network leakage

Registered HTTP routes return deterministic responses through the local proxy. Unregistered routes are recorded as schema breaches. HTTPS `CONNECT` is terminated with an ephemeral per-host certificate; clients must use an isolated trust policy, and the E2E client disables verification because the mock certificate is not a public trust root.

### Optional local decision model

The optional `laya` Rust feature performs inference from a checkpoint directory already present on
the machine. `dry-exec` does not fetch model weights during evaluation, and no model API is used as
a fallback. Checkpoint acquisition is an operator-controlled step and should be verified and
performed before an air-gapped run. If loading or inference fails, the caller must use the
deterministic decision path; neural output never overrides a deterministic refusal.

## Threat & Isolation Model

`dry-exec` provides an ephemeral execution primitive engineered to intercept and record state mutations produced by autonomous execution loops before changes commit to durable environments.

### Core Isolation Boundaries
1. **Namespace Isolation**:
   - `CLONE_NEWPID`: Child processes cannot signal, trace, or observe host processes.
   - `CLONE_NEWNET`: Default loopback-only environment; external interfaces are unconfigured, forcing network requests through the transparent proxy.
   - `CLONE_NEWNS`: Mount table is unshared with `MS_REC | MS_PRIVATE`. Sterile `/proc` and private `tmpfs` mounts prevent host filesystem alteration.
   - `CLONE_NEWIPC` & `CLONE_NEWUTS`: IPC queues and system hostnames are isolated.

2. **Syscall Interception (`seccomp-bpf`)**:
   - Evaluates system calls against an explicit whitelist.
   - Disallowed system calls generate `SECCOMP_RET_TRAP` (triggering `SIGSYS`), causing the isolation boundary to intercept the violation, record the instruction pointer and syscall number, and abort execution.

3. **Memory & State Mutation Tracking**:
   - Processes execute against Copy-on-Write (CoW) memory mappings (`MAP_PRIVATE`).
   - Mutations are captured via kernel soft-dirty bitfields and ephemeral inode deltas, ensuring host storage is never directly modified during dry-run exploration.

## Reporting Vulnerabilities

If you identify an escape vector, namespace leak, or memory-boundary defect:
1. **Do not open a public GitHub issue.**
2. Email full reproduction details, kernel version, and PoC script to `security@dry-exec.org`.
3. Allow up to 48 hours for an initial triage and confirmation from the core maintainers.
4. Coordinated disclosure will occur following the release of a verified patch.
