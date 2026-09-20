<div align="center">

# dex (dry-exec)

**Ephemeral kernel-level execution for autonomous loops.**

Explore state mutations without consequences. Commit only what you approve.

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.70+-orange.svg)](https://www.rust-lang.org/)
[![Python](https://img.shields.io/badge/Python-3.10+-blue.svg)](https://www.python.org/)
[![Platform](https://img.shields.io/badge/Platform-Linux-green.svg)](#verification)
[![CI](https://github.com/Devaretanmay/dry-exec/actions/workflows/ci.yml/badge.svg)](https://github.com/Devaretanmay/dry-exec/actions/workflows/ci.yml)

</div>

---

## What is dex?

`dex` is a **deterministic state exploration primitive** for autonomous execution loops. It intercepts proposed mutations, runs them inside ephemeral Linux kernel namespaces, and returns the exact byte-level state delta — without ever touching your real environment.

Think of it as `git diff` for arbitrary runtime state: memory, filesystem, and network — computed at the kernel level.

### Why?

| Problem | dex |
|---|---|
| Autonomous loops mutate state blindly | Every mutation runs in an isolated namespace first |
| Rollback is expensive and error-prone | Nothing to roll back — baseline is never touched |
| State diffing requires app-level hashing | Kernel soft-dirty pagemap tracking: $O(P_{\text{dirty}})$ |
| Network calls leak to production | Transparent proxy returns deterministic mock responses |

---

## Installation

```bash
pip install dry-exec
```

---

## Quickstart

### Path 1: The `dex` CLI (Direct Command Execution)

Run any command in an ephemeral sandbox. No YAML required:

```bash
# Dry-run: inspect the state delta without modifying host
dex "python migrate.py"

# Commit changes only if the dry-run delta looks right
dex --commit "npm run seed"

# Inspect active environment limits and boundary rules
dex inspect --config env.yaml
```

<br>

### Path 2: The `@dex.dry_run` Decorator

Wrap any function to run inside an ephemeral kernel sandbox. Returns the computed `StateDelta`:

```python
import dex

@dex.dry_run
def update_balance(account_id: str, amount: int):
    # Runs in ephemeral sandbox; database is untouched
    db.execute("UPDATE accounts SET balance = balance + $1 WHERE id = $2", amount, account_id)

delta = update_balance("acc_101", 500)
print(f"{delta.total_bytes_mutated} bytes mutated in {delta.duration_nanos}ns")
```

Or execute directly with functional `dex.run()`:

```python
delta = dex.run("python seed_data.py")
```

<br>

### Path 3: The 3-Line Autonomous Agent Loop

Let an autonomous execution loop propose actions, test them ephemerally, evaluate state deltas, and self-correct:

```python
import asyncio
from dex import Agent

async def main():
    agent = Agent(task="Clean up old temp files and migrate user records")
    result = await agent.run()

    print(f"Success: {result.success}, Trials: {result.trials_conducted}, Committed: {result.committed}")

asyncio.run(main())
```

---

## How it works

```
 Autonomous Execution Loop (Python SDK)
 ┌──────────────────────────────────────────┐
 │  @dex.dry_run or Agent(task=...).run()   │
 │  Async FFI dispatch (GIL released)       │
 │  Structured telemetry + OTel spans       │
 └──────────────┬───────────────────────────┘
                │ PyO3 FFI
                ▼
 Rust Control Plane
 ┌──────────────────────────────────────────┐
 │  CoW memory mapping (mmap MAP_PRIVATE)   │
 │  /proc/[pid]/clear_refs baseline         │
 │  Transparent proxy for mock endpoints    │
 └──────────────┬───────────────────────────┘
                │ clone(NEWPID|NEWNET|NEWNS)
                ▼
 Ephemeral Execution Layer
 ┌──────────────────────────────────────────┐
 │  Isolated PID/NET/MNT/IPC namespaces     │
 │  seccomp-BPF syscall interception        │
 │  Hardware CoW (only dirty pages copied)  │
 │  tmpfs overlay for filesystem mutations  │
 └──────────────────────────────────────────┘
                │
                ▼
        StateDelta { memory, fs, network }
```

---

## Features

- **Direct `dex` CLI** — Run any command ephemerally without configuration files
- **`@dex.dry_run` decorator** — Zero boilerplate ephemeral execution for Python functions
- **3-line agent loop** — Propose → dry-run → evaluate → self-correct → commit
- **Kernel-level isolation** — Linux namespaces (PID, NET, MNT, IPC, UTS) with seccomp-BPF syscall filtering
- **$O(P_{\text{dirty}})$ state diffing** — Soft-dirty pagemap tracking, no app-level hashing
- **Transparent network proxy** — Schema-driven mock responses, zero external egress
- **OpenTelemetry integration** — Structured JSON receipts and OTel span tracing
- **Rich terminal visualization** — `DeltaLogger` renders memory/fs/network mutations

---

## Examples

| Example | Description |
|---|---|
| [`quickstart.py`](examples/getting_started/quickstart.py) | 3-line setup with `@dex.dry_run` |
| [`native_agent.py`](examples/getting_started/native_agent.py) | 3-line self-correcting agent loop |
| [`type_safe_db_migration.py`](examples/use_cases/type_safe_db_migration.py) | DB migration recovering from schema boundary rejections |
| [`api_payment_exploration.py`](examples/use_cases/api_payment_exploration.py) | Payment API exploration with mock interception |
| [`langchain_tool.py`](examples/integrations/langchain_tool.py) | LangChain tool wrapper |

---

## Verification

`dex` requires Linux kernel primitives. On macOS/Windows, run the container harness:

```bash
./scripts/test.sh
```

Or via Docker directly:

```bash
./docker/build.sh
docker run --rm --privileged --cap-add=SYS_ADMIN --cap-add=SYS_PTRACE dry-exec-test-harness
```

---

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup and terminology conventions.

## License

[MIT](LICENSE)
