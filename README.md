<div align="center">

# dry-exec

**Deterministic ephemeral execution primitive for autonomous AI loops.**

Explore state mutations without consequences. Commit only what you approve.

[![PyPI](https://img.shields.io/pypi/v/dry-exec.svg)](https://pypi.org/project/dry-exec/)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.80+-orange.svg)](https://www.rust-lang.org/)
[![Python](https://img.shields.io/badge/Python-3.10+-blue.svg)](https://www.python.org/)
[![Platform](https://img.shields.io/badge/Platform-Linux%20%7C%20macOS-green.svg)](#architecture-dual-backend-engine)
[![CI](https://github.com/Devaretanmay/dry-exec/actions/workflows/ci.yml/badge.svg)](https://github.com/Devaretanmay/dry-exec/actions/workflows/ci.yml)

</div>

---

## What is dry-exec?

One AI agent is a cool demo. Running 100 autonomous agents in production is a state-management nightmare:

- Commands modify host files before anyone reviews them.
- Migration scripts touch the wrong database table or lock schemas.
- External API calls leak real credentials, spam webhooks, or trigger real billing.
- Cleaning up failed agent attempts turns into an emergency pager rotation.

`dry-exec` gives every proposed action an ephemeral trial run. The action executes behind native OS boundaries (Linux namespaces or macOS Seatbelt + APFS copy-on-write). The SDK captures every mutation and returns a structured `StateDelta` receipt containing modified filesystem inodes, network calls, stdout, stderr, and exit codes.

The mental model: **`git diff` for runtime execution.**

### What it covers

| Production risk | Ephemeral trial outcome |
|---|---|
| Unreviewed filesystem writes | Isolated filesystem mutation list (inode diffing) |
| Failed commands hidden behind wrappers | Captured stdout, stderr, and real exit code |
| Live API calls during exploration | Schema-driven loopback mock proxy with zero outbound egress |
| Host mutations during dry-runs | Linux namespaces or macOS Seatbelt/APFS CoW boundaries |
| Autonomous decisions from raw text | Typed `StateDelta` receipt and $O(1)$ decision routing |
| Deferred execution escapes (.git/hooks, shell rc, .pth) | Categorical violation routing via deferred execution scanner |
| Real secret exfiltration during execution | Phantom token masking across stdout, stderr, network, and receipts |
| Uncontrolled tool side-effects | Zero-dependency MCP sandbox wrapper (`@sandboxed_mcp_tool`) |
| Unclean state recovery on failure | Atomic `Trial` primitive with rollback by default |

---

## Installation

Install directly from PyPI:

```bash
pip install dry-exec
```

Or lock to a specific release:

```bash
pip install dry-exec==0.1.0
```

---

## Quickstart

### 1. The 3-line autonomous agent loop

Let an autonomous agent propose actions, dry-run them ephemerally, evaluate state deltas, and self-correct if a boundary is violated:

```python
import asyncio
from dry_exec import Agent

async def main():
    agent = Agent(task="Clean up old temp files and migrate user records")
    result = await agent.run()

    print(f"Success: {result.success}, Trials: {result.trials_conducted}, Committed: {result.committed}")

asyncio.run(main())
```

`result.committed` reflects the decision layer's routing: `True` means the delta was cleared for auto-commit, not that the Agent replayed mutations to durable storage. Durable commit remains the caller's responsibility.

<br>

### 2. Direct ephemeral client execution

Run an isolated trial, inspect the exact `StateDelta` receipt, and review decisions before approving state mutations:

```python
import asyncio
from dry_exec import Action, DryExecClient, Environment

async def main():
    client = DryExecClient()
    env = Environment(name="sandbox", allowed_mutation_targets={"*"})
    action = Action(
        action_id="act_001",
        target_resource="shell_command",
        mutation_type="execute",
        payload={"command": ["/bin/sh", "-c", "echo hello > test.txt"]},
    )

    delta = await client.execute_ephemeral_action(env, action)
    print(f"Decision: {delta.decision.choice}, Risk: {delta.decision.risk_score}")
    print(f"Filesystem mutations: {delta.fs_mutations}")

asyncio.run(main())
```

Commands execute with their working directory set to the ephemeral root. Absolute paths outside that root are read-only inside the Linux mount namespace.

<br>

### 3. The `@dry_exec.dry_run` decorator

Wrap any Python function to run inside an ephemeral kernel sandbox. Returns the computed `StateDelta`:

```python
import dry_exec

@dry_exec.dry_run
def update_balance(account_id: str, amount: int):
    # Self-contained: the function is serialized and executed in the isolated child process.
    with open("/tmp/dry_exec_ephemeral/balance_receipt.txt", "w") as receipt:
        receipt.write(f"{account_id}:{amount}")

delta = update_balance("acc_101", 500)
print(f"{delta.total_bytes_mutated} bytes mutated in {delta.duration_nanos}ns")
```

The decorator serializes the function and its arguments; helpers, globals, and database handles from the parent process are not available inside the boundary. Use `Action.payload["command"]` for arbitrary commands; other payload keys remain schema metadata.

Or execute arbitrary commands directly via functional `dry_exec.run()`:

```python
delta = dry_exec.run("python seed_data.py")
```

<br>

### 4. Atomic Trials: commit or rollback

Test modifications against a target workspace directory. If approved, mutations are applied atomically via temporary file replacement; otherwise, the scratch workspace is discarded with zero host mutations:

```python
import asyncio
from dry_exec import Action, Choice, DryExecClient, Environment

async def main():
    client = DryExecClient()
    env = Environment()
    action = Action(
        action_id="act_migrate",
        target_resource="app_code",
        mutation_type="execute",
        payload={"command": ["/bin/sh", "-c", "echo 'v2' > config.txt"]},
    )

    async with client.trial(env, action, workspace_dir="./my_project") as trial:
        print(f"Decision: {trial.decision.choice}, Hazards: {trial.delta.deferred_execution_hazards}")
        if trial.decision.choice == Choice.ALLOWED:
            await trial.commit()  # Atomically applies mutations to ./my_project
        # If uncommitted upon context exit, trial automatically rolls back

asyncio.run(main())
```

<br>

### 5. Sandboxed MCP tools (Model Context Protocol)

Execute Model Context Protocol tools and agent tool functions inside kernel-level isolation with zero external runtime dependencies:

```python
import asyncio
from dry_exec import sandboxed_mcp_tool, SandboxedToolResult

@sandboxed_mcp_tool(auto_commit=True)
async def execute_query(sql: str) -> str:
    # Runs inside isolated boundary; persistent hook mutations trigger categorical escalation
    return "Query executed successfully"

async def main():
    res: SandboxedToolResult = await execute_query("SELECT 1")
    print(f"Tool output: {res.result}, Committed: {res.committed}")

asyncio.run(main())
```

---

## Architecture: dual-backend engine

`dry-exec` features a **native dual-backend architecture**. It automatically detects the host operating system and compiles to native kernel primitives. Decision routing is sub-millisecond; end-to-end latency is dominated by the executed action:

```
                            dry-exec architecture
                                       │
            ┌──────────────────────────┴──────────────────────────┐
            ▼                                                     ▼
      Linux Backend                                         macOS Backend
   (Production & Cloud)                                  (Local Development)
─────────────────────────                             ─────────────────────────
• Linux Namespaces (PID, NET, MNT)                    • Apple Seatbelt (sandbox_init)
• Read-only host remount + sterile /proc              • APFS Copy-on-Write (clonefile)
• seccomp-BPF TRAP filter                             • Transparent Loopback Proxy
• /proc/[pid]/pagemap soft-dirty
• Zero Docker needed                                  • Zero Docker needed
```

```
  Autonomous Execution Loop (Python SDK)
  ┌──────────────────────────────────────────────┐
  │  @dry_exec.dry_run or Agent(task=...).run()  │
  │  Async FFI dispatch (Python GIL released)    │
  └──────────────┬───────────────────────────────┘
                 │ PyO3 FFI
                 ▼
  Rust Control Plane (Linux & macOS Router)
  ┌──────────────────────────────────────────────┐
  │  Linux: Namespaces + seccomp + pagemap       │
  │  macOS: Seatbelt MAC + APFS CoW clone        │
  │  Transparent proxy for mock endpoints        │
  │  System-One Decision Layer (O(1) in delta)   │
  └──────────────┬───────────────────────────────┘
                 │
                 ▼
         StateDelta { memory, fs, network, decision }
```

---

## Features

- **3-line agent loop**: Propose → dry-run → evaluate → self-correct → commit.
- **`@dry_exec.dry_run` decorator**: Zero-boilerplate ephemeral execution for Python functions.
- **Kernel-level Linux isolation**: Linux namespaces (PID, NET, MNT, IPC, UTS) with seccomp-BPF filtering.
- **Native macOS support**: Kernel-level Seatbelt MAC sandboxing and APFS `clonefile` hardware CoW.
- **$O(P_{\text{dirty}})$ state diffing**: Soft-dirty pagemap tracking with no application-level hashing overhead.
- **Transparent network proxy**: Schema-driven mock responses with zero external egress.
- **Deterministic System-One decision engine**: $O(1)$ scalar projection routing safe actions to Auto-Commit in nanoseconds.

### Optional local System-One scorer

`dry-exec-core` includes an opt-in `laya` feature for local Laya inference through the published
Rust `laya-rs` runtime. The feature loads a checkpoint from a caller-provided local directory;
it never downloads weights implicitly. Deterministic boundary decisions remain first: blocked or
violated actions do not invoke Laya, while allowed actions may blend deterministic risk (40%) with
the local model score (60%). If checkpoint loading fails, callers can retain deterministic-only
routing.

```bash
cargo check -p dry-exec-core --features laya
```

---

## Production patterns

- [Safe database migrations](docs/patterns/safe-db-migrations.md): Test schema migrations against an ephemeral boundary before applying changes to durable storage.
- [Preventing API leakage](docs/patterns/preventing-api-leakage.md): Intercept external API calls with deterministic mock responses and detect unapproved outbound requests.

---

## Examples

| Example | Description |
|---|---|
| [`quickstart.py`](examples/getting_started/quickstart.py) | 3-line setup with `@dry_exec.dry_run` |
| [`native_agent.py`](examples/getting_started/native_agent.py) | 3-line self-correcting agent loop |
| [`type_safe_db_migration.py`](examples/type_safe_db_migration.py) | Database migration recovering from schema boundary rejections |
| [`api_payment_exploration.py`](examples/api_payment_exploration.py) | Payment API exploration with mock interception |
| [`langchain_tool.py`](examples/integrations/langchain_tool.py) | LangChain tool wrapper |

---

## Verification

### Native local verification (Linux and macOS)

Run the local test harness directly on your machine (zero Docker required):

```bash
./scripts/test.sh
```

### Containerized Linux verification (cross-platform)

To test the complete Linux kernel primitive suite inside Docker:

```bash
./docker/build.sh
docker run --rm --privileged --cap-add=SYS_ADMIN --cap-add=SYS_PTRACE dry-exec-test-harness
```

---

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup, architectural terminology, and guidelines.

## License

[MIT](LICENSE)
