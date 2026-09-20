"""15-line native agent loop demonstration with self-correction within ephemeral boundaries."""

import asyncio
import json
from dry_exec import DryExecAgent, Environment


def mock_self_correcting_llm(task: str, history: list) -> str:
    """Simulates an LLM loop that makes an initial invalid mutation before self-correcting."""
    last_prompt = history[-1]["content"] if history else ""
    if "Schema boundary violation" in last_prompt or "Execution failed" in last_prompt:
        # Self-correction after observing pre-execution schema rejection
        return json.dumps({
            "action_id": "act_retry_02",
            "target_resource": "user_status",
            "mutation_type": "update",
            "payload": {"status": "verified"},
        })
    # Initial invalid attempt targeting unauthorized resource
    return json.dumps({
        "action_id": "act_initial_01",
        "target_resource": "admin_credentials",  # Rejected by schema boundary
        "mutation_type": "overwrite",
        "payload": {"role": "root"},
    })


async def main():
    env = Environment(
        name="onboarding_environment",
        allowed_mutation_targets={"user_status"},
        memory_limit_bytes=1048576,
    )
    agent = DryExecAgent(environment=env, llm_caller=mock_self_correcting_llm, max_retries=3)
    result = await agent.run(task="Promote newly registered user to verified status", auto_commit=True)
    print(f"Agent Loop Finished: success={result.success}, trials={result.trials_conducted}, committed={result.committed}")


if __name__ == "__main__":
    asyncio.run(main())
