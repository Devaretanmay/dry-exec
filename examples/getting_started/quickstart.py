"""Basic 10-line getting started usage of dry-exec."""

import asyncio
from dry_exec import Action, DryExecClient, Environment


async def main():
    # 1. Define the isolated environment boundary
    env = Environment(
        name="quickstart_env",
        allowed_mutation_targets={"system_status"},
        memory_limit_bytes=4096,
    )

    # 2. Define an ephemeral action adhering to the schema
    action = Action(
        action_id="act_quickstart_01",
        target_resource="system_status",
        mutation_type="update",
        payload={"status": "online"},
    )

    # 3. Execute dry-run exploration and observe deterministic state mutations
    client = DryExecClient()
    delta = await client.execute_ephemeral_action(env, action)
    print(f"Ephemeral execution verified: {delta.total_bytes_mutated} bytes mutated in {delta.duration_nanos} ns.")


if __name__ == "__main__":
    asyncio.run(main())
