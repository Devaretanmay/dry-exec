# Security & Kernel Isolation Model

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
