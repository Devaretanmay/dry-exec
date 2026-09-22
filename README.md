<div align="center">

# dry-exec

**Deterministic ephemeral execution primitive for autonomous loops.**

Explore state mutations without consequences. Commit only what you approve.

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.70+-orange.svg)](https://www.rust-lang.org/)
[![Python](https://img.shields.io/badge/Python-3.10+-blue.svg)](https://www.python.org/)
[![Platform](https://img.shields.io/badge/Platform-Linux%20%7C%20macOS-green.svg)](#architecture)
[![CI](https://github.com/Devaretanmay/dry-exec/actions/workflows/ci.yml/badge.svg)](https://github.com/Devaretanmay/dry-exec/actions/workflows/ci.yml)

</div>

---

## What is dry-exec?

One AI agent is a cool demo. Running 100 in production is a state-management problem:

- commands can write files before review;
- migration code can touch the wrong database;
- API calls can leak real credentials or spend real money;
- rollback becomes someone’s pager.

`dry-exec` gives each proposed action an ephemeral trial. The action runs behind native Linux or macOS boundaries. The SDK returns a structured `StateDelta` containing observed memory, filesystem, network, stdout, stderr, and exit-code changes. You review that receipt before committing anything durable.

The useful mental model: `git diff` for a runtime action.

### What it covers

| Production risk | Trial result |
|---|---|
| Unreviewed filesystem writes | Isolated filesystem mutation list |
| Failed command hidden behind a wrapper | Captured stdout, stderr, and exit code |
| Real API call during exploration | Schema-driven HTTP mock proxy |
| Host changes during a dry run | Linux namespaces or macOS Seatbelt/APFS boundaries |
| Agent decision made from raw logs | Typed `StateDelta` and decision receipt |

Linux real-command execution and HTTP/HTTPS mock interception are verified in the container harness. macOS `/bin/echo` real-command execution is covered by native E2E.

---

## Installation

```bash
pip install dry-exec
```

---

## Quickstart

### Path 1: The `dex` CLI (Direct Command Execution)

Run any command directly in an ephemeral sandbox without YAML or Docker (`de` and `dry-exec` also supported as aliases):

```bash
# Dry-run: inspect the state delta without modifying host
dex "python migrate.py"

# Commit changes only if the dry-run delta looks right
dex --commit "npm run seed"

# Inspect active environment limits and boundary rules
dex inspect --config env.yaml
```

```
$ dex "python dangerous_script.py"

╭──────────────── Ephemeral State Exploration: Proposed Action ────────────────╮
│ Environment     cli_ephemeral_sandbox                                        │
│ Action ID       cmd_7f2b1a                                                   │
│ Target Resource shell_command                                                │
│ Mutation Type   execute                                                      │
│ Payload         {'command': 'python dangerous_script.py'}                    │
╰──────────────────── Pre-Execution Boundary Verification ─────────────────────╯
╭─────────────────────── State Delta Receipt (Trial #1) ───────────────────────╮
│ Metric                           Count / Value                               │
│ ──────────────────────────────────────────────────────────────────────────── │
│ Memory Pages Mutated             N/A (macOS bare-metal; Linux pagemap only)  │
│ Filesystem Changes               3 inodes (APFS CoW snapshot)                │
│ Network Requests Intercepted     1 request intercepted (Stripe mock proxy)   │
│ Total Mutated Bytes              4,096 bytes                                 │
│ Computation Latency              0.320 ms (sub-millisecond native kernel)    │
╰──────────────────────────────────────────────────────────────────────────────╯

Commit state delta to target environment? [y/N]: n
[Real host and live APIs remain untouched.]
```

<br>

### Path 2: The `@dry_exec.dry_run` Decorator

Wrap any function to run inside an ephemeral kernel sandbox. Returns the computed `StateDelta`:

```python
import dry_exec

@dry_exec.dry_run
def update_balance(account_id: str, amount: int):
    # Runs in ephemeral sandbox; database is untouched
    db.execute("UPDATE accounts SET balance = balance + $1 WHERE id = $2", amount, account_id)

delta = update_balance("acc_101", 500)
print(f"{delta.total_bytes_mutated} bytes mutated in {delta.duration_nanos}ns")
```

Or execute directly with functional `dry_exec.run()`:

```python
delta = dry_exec.run("python seed_data.py")
```


<br>

### Path 3: The 3-Line Autonomous Agent Loop

Let an autonomous execution loop propose actions, test them ephemerally, evaluate state deltas, and self-correct:

```python
import asyncio
from dry_exec import Agent

async def main():
    agent = Agent(task="Clean up old temp files and migrate user records")
    result = await agent.run()

    print(f"Success: {result.success}, Trials: {result.trials_conducted}, Committed: {result.committed}")

asyncio.run(main())
```

---

## Architecture: Dual-Backend Engine

`dry-exec` features a **native dual-backend architecture**. It automatically detects the host operating system and compiles to native kernel primitives with sub-millisecond execution latencies:

```
                            dry-exec Architecture
                                      │
            ┌─────────────────────────┴─────────────────────────┐
            ▼                                                   ▼
      Linux Backend                                       macOS Backend
   (Production & Cloud)                                (Local Development)
─────────────────────────                           ─────────────────────────
• Linux Namespaces (PID, NET, MNT)                  • Apple Seatbelt (sandbox_init)
• seccomp-BPF TRAP filter                           • APFS Copy-on-Write (clonefile)
• /proc/[pid]/pagemap soft-dirty                    • Transparent Loopback Proxy
• Zero Docker needed                                • Zero Docker needed
```

```
  Autonomous Execution Loop (Python SDK)
  ┌──────────────────────────────────────────────┐
  │  @dry_exec.dry_run or Agent(task=...).run()  │
  │  Async FFI dispatch (GIL released)           │
  │  Structured telemetry + OTel spans           │
  └──────────────┬───────────────────────────────┘
                 │ PyO3 FFI / `dex` CLI
                 ▼
  Rust Control Plane (Linux & macOS Router)
  ┌──────────────────────────────────────────────┐
  │  Linux: Namespaces + seccomp + pagemap       │
  │  macOS: Seatbelt MAC + APFS CoW clone        │
  │  Transparent proxy for mock endpoints        │
  └──────────────┬───────────────────────────────┘
                 │
                 ▼
         StateDelta { memory, fs, network }
```

---

## Features

- **Direct `dex` CLI** — Run any command ephemerally without configuration files or Docker (`de` and `dry-exec` aliases supported)
- **`@dry_exec.dry_run` decorator** — Zero boilerplate ephemeral execution for Python functions
- **3-line agent loop** — Propose → dry-run → evaluate → self-correct → commit
- **Native macOS support** — Kernel-level Seatbelt MAC sandboxing and APFS `clonefile` hardware CoW

- **Kernel-level Linux isolation** — Linux namespaces (PID, NET, MNT, IPC, UTS) with seccomp-BPF filtering
- **$O(P_{\text{dirty}})$ state diffing** — Soft-dirty pagemap tracking, no app-level hashing
- **Transparent network proxy** — Schema-driven mock responses, zero external egress
- **OpenTelemetry integration** — Structured JSON receipts and OTel span tracing
- **Rich terminal visualization** — `DeltaLogger` renders memory/fs/network mutations

### Optional local System-One scorer

`dry-exec-core` includes an opt-in `laya` feature for local Laya inference through the published
Rust `laya-rs` runtime. The feature loads a checkpoint from a caller-provided local directory;
it never downloads weights implicitly. Deterministic boundary decisions remain first: blocked or
violated actions do not invoke Laya, while allowed actions may blend deterministic risk (40%) with
the local model score (60%). If checkpoint loading fails, callers can retain deterministic-only
routing. This feature is disabled by default because Laya checkpoints are large and latency must
be measured on the target hardware rather than assumed from upstream benchmarks.

```bash
cargo check -p dry-exec-core --features laya
```

---

## Examples

| Example | Description |
|---|---|
| [`quickstart.py`](examples/getting_started/quickstart.py) | 3-line setup with `@dry_exec.dry_run` |
| [`native_agent.py`](examples/getting_started/native_agent.py) | 3-line self-correcting agent loop |
| [`type_safe_db_migration.py`](examples/use_cases/type_safe_db_migration.py) | DB migration recovering from schema boundary rejections |
| [`api_payment_exploration.py`](examples/use_cases/api_payment_exploration.py) | Payment API exploration with mock interception |
| [`langchain_tool.py`](examples/integrations/langchain_tool.py) | LangChain tool wrapper |


---

## Verification

### Native Local Verification (Linux & macOS)

Run the local test harness directly on your machine (zero Docker required):

```bash
./scripts/test.sh
```

### Containerized Linux Verification (Cross-Platform)

To test the complete Linux kernel primitive suite inside Docker:

```bash
./docker/build.sh
docker run --rm --privileged --cap-add=SYS_ADMIN --cap-add=SYS_PTRACE dry-exec-test-harness
```

---

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup and terminology conventions.

## License

[MIT](LICENSE)
