# Safe database migrations

Letting an autonomous agent run database migrations is terrifying: a single bad column name, missing index, or unvetted `ALTER TABLE` locks tables or corrupts production state.

`dry-exec` gives migration scripts an ephemeral sandbox run before touching your production database. It intercepts filesystem writes, monitors exit codes, and provides a structured `StateDelta` receipt so you or your automated loop can inspect every side effect before applying changes.

## Quick example

Run migration logic in an ephemeral sandbox and inspect the outcome:

```python
import asyncio
from dry_exec import Action, DryExecClient, Environment

async def run_trial():
    # 1. Define the ephemeral environment boundary
    env = Environment(
        name="migration-review",
        allowed_mutation_targets={"schema", "migration_lock"},
        allowed_filesystem_roots=["/tmp/dry_exec_ephemeral"],
    )

    # 2. Propose the migration action
    action = Action(
        action_id="add-index-v1",
        target_resource="schema",
        mutation_type="execute",
        payload={"command": ["python3", "migrations/add_index.py"]},
    )

    # 3. Execute in ephemeral kernel isolation
    delta = await DryExecClient().execute_ephemeral_action(env, action)

    # 4. Inspect the execution receipt
    if delta.exit_code != 0:
        raise RuntimeError(f"Migration trial failed: {delta.stderr.decode('utf-8')}")

    print(f"Decision: {delta.decision.choice} (Risk: {delta.decision.risk_score})")
    for mutation in delta.fs_mutations:
        print(f"Filesystem: {mutation.mutation_type} -> {mutation.path}")

asyncio.run(run_trial())
```

## How it works under the hood

1. **Kernel isolation**: On Linux, the migration runs inside private mount, PID, and network namespaces. On macOS, it executes behind an Apple Seatbelt sandbox with APFS copy-on-write `clonefile` tracking.
2. **Deterministic receipt**: You get an exact list of modified inodes, exit code, captured stdout/stderr, and risk evaluation.
3. **Rollback-free verification**: If the migration crashes, attempts unauthorized network calls, or writes outside `/tmp/dry_exec_ephemeral`, the trial is aborted cleanly without modifying your durable database or host filesystem.

> **Good to know**: While `dry-exec` isolates process execution and tracks state mutations, it works best alongside transactional DDL, automated schema linters, and reliable database backups.
