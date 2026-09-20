"""Production-ready example: Self-correcting database migration within an ephemeral execution boundary."""

import asyncio
from dry_exec import (
    Action,
    DryExecClient,
    Environment,
    SchemaViolationError,
    StateDelta,
)
from dry_exec.observability import DeltaLogger

logger = DeltaLogger()


async def run_self_correcting_db_migration():
    """Demonstrates an autonomous execution loop self-correcting after a schema boundary rejection."""
    # 1. Define immutable environment boundary
    env = Environment(
        name="production_payments_db",
        allowed_mutation_targets={"balance", "ledger_entry"},
        allowed_filesystem_roots=["/tmp/dry_exec_ephemeral"],
        memory_limit_bytes=16 * 1024 * 1024,
    )

    client = DryExecClient()

    # 2. Step 1: Execution loop attempts an unauthorized mutation (typo targeting 'user_role')
    faulty_action = Action(
        action_id="mig_001_initial",
        target_resource="user_role",  # Violates environment schema boundary
        mutation_type="execute_sql",
        payload={"query": "UPDATE users SET role = 'admin' WHERE id = 42;"},
    )

    logger.render_action_header(env, faulty_action)

    try:
        await client.execute_ephemeral_action(env, faulty_action)
    except SchemaViolationError as exc:
        # Pre-execution boundary rejected the action synchronously
        logger.render_violation(exc)

    # 3. Step 2: Autonomous execution loop adapts control flow based on boundary telemetry
    corrected_action = Action(
        action_id="mig_002_corrected",
        target_resource="balance",  # Valid, schema-compliant target
        mutation_type="execute_sql",
        payload={"query": "UPDATE accounts SET balance = balance + 500 WHERE account_id = 'ACC_99';"},
    )

    logger.render_action_header(env, corrected_action)

    # 4. Step 3: Execute inside ephemeral kernel isolation
    delta: StateDelta = await client.execute_ephemeral_action(env, corrected_action)

    # 5. Step 4: Render verifiable receipt of exact state mutation
    logger.render_delta(delta, trial_id=2)
    logger.render_commit_prompt(confirmed=True)

    print("\n[SUCCESS] Self-correcting state exploration workflow verified.")


if __name__ == "__main__":
    asyncio.run(run_self_correcting_db_migration())
