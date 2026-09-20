<div align="center">

# dry-exec

**Ephemeral kernel-level execution for autonomous loops.**

Explore state mutations without consequences. Commit only what you approve.

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.70+-orange.svg)](https://www.rust-lang.org/)
[![Python](https://img.shields.io/badge/Python-3.10+-blue.svg)](https://www.python.org/)
[![Linux](https://img.shields.io/badge/Platform-Linux-green.svg)](#verification)
[![CI](https://github.com/Devaretanmay/dry-exec/actions/workflows/ci.yml/badge.svg)](https://github.com/Devaretanmay/dry-exec/actions/workflows/ci.yml)

</div>

---

## What is dry-exec?

`dry-exec` is a **deterministic state exploration primitive** for autonomous execution loops. It intercepts proposed mutations, runs them inside ephemeral Linux kernel namespaces, and returns the exact byte-level state delta — without ever touching your real environment.

Think of it as `git diff` for arbitrary runtime state: memory, filesystem, and network — computed at the kernel level.

### Why?

| Problem | dry-exec |
|---|---|
| Autonomous loops mutate state blindly | Every mutation runs in an isolated namespace first |
| Rollback is expensive and error-prone | Nothing to roll back — baseline is never touched |
| State diffing requires app-level hashing | Kernel soft-dirty pagemap tracking: $O(P_{\text{dirty}})$ |
| Network calls leak to production | Transparent proxy returns deterministic mock responses |

---

## Quickstart

### Install

```bash
pip install dry-exec
```

### Run your first ephemeral execution

```python
import asyncio
from dry_exec import Action, DryExecClient, Environment

async def main():
    env = Environment(
        name="quickstart_env",
        allowed_mutation_targets={"system_status"},
    )

    action = Action(
        action_id="act_001",
        target_resource="system_status",
        mutation_type="update",
        payload={"status": "online"},
    )

    client = DryExecClient()
    delta = await client.execute_ephemeral_action(env, action)
    print(f"{delta.total_bytes_mutated} bytes mutated in {delta.duration_nanos}ns")

asyncio.run(main())
```

### Use the built-in agent loop

```python
from dry_exec import DryExecAgent, Environment

env = Environment(
    name="db_migration",
    allowed_mutation_targets={"users_table", "schema_version"},
)

agent = DryExecAgent(environment=env, max_retries=3)
result = await agent.run("Migrate users table to v2 schema")

print(result)  # success=True, trials=2, committed=True
```

---

## How it works

```
 Autonomous Execution Loop (Python SDK)
 ┌──────────────────────────────────────────┐
 │  Pydantic V2 schema validation           │
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

**Result:** You get a deterministic `StateDelta` containing every mutated memory page, filesystem inode, and intercepted network request — with zero changes to the baseline.

---

## Features

- **Kernel-level isolation** — Linux namespaces (PID, NET, MNT, IPC, UTS) with seccomp-BPF syscall filtering
- **$O(P_{\text{dirty}})$ state diffing** — Soft-dirty pagemap tracking, no app-level hashing
- **Transparent network proxy** — Schema-driven mock responses, zero external egress
- **Type-safe Python SDK** — Pydantic V2 schemas, async-first client, structured exceptions
- **Built-in agent loop** — Propose → dry-run → evaluate → self-correct → commit
- **OpenTelemetry integration** — Structured JSON receipts and OTel span tracing
- **Rich terminal visualization** — `DeltaLogger` renders memory/fs/network mutations
- **Developer CLI** — `dry-exec run`, `dry-exec inspect`, `dry-exec version`

---

## Examples

| Example | Description |
|---|---|
| [`quickstart.py`](examples/getting_started/quickstart.py) | Minimal setup in 10 lines |
| [`native_agent.py`](examples/getting_started/native_agent.py) | Self-correcting agent loop with OpenAI SDK |
| [`type_safe_db_migration.py`](examples/use_cases/type_safe_db_migration.py) | DB migration recovering from schema boundary rejections |
| [`api_payment_exploration.py`](examples/use_cases/api_payment_exploration.py) | Payment API exploration with mock interception |
| [`langchain_tool.py`](examples/integrations/langchain_tool.py) | LangChain tool wrapper |

---

## Project structure

```
dry-exec/
├── crates/
│   ├── dry-exec-core/          # Rust: namespaces, seccomp, CoW, pagemap, proxy
│   └── dry-exec-pyo3/          # PyO3 FFI bridge
├── python/dry_exec/            # Python SDK: client, agent, schemas, telemetry
├── examples/                   # Getting started, use cases, integrations
├── tests/                      # SDK tests, CLI tests, container harness
├── scripts/                    # build.sh, lint.sh, test.sh
├── docker/                     # CI container with Linux kernel primitives
└── .github/workflows/          # CI, lint, release pipelines
```

---

## Verification

`dry-exec` requires Linux kernel primitives. On macOS/Windows, use the container harness:

```bash
./scripts/test.sh
```

Or run directly:

```bash
./docker/build.sh
docker run --rm --privileged --cap-add=SYS_ADMIN --cap-add=SYS_PTRACE dry-exec-test-harness
```

---

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup and terminology conventions.

## License

[MIT](LICENSE)
