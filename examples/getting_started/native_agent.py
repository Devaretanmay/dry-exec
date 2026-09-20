"""Standalone autonomous execution loop with OpenAI SDK and dry-exec self-correction."""

import asyncio
import os
from typing import List, Dict
from dry_exec import Action, DryExecAgent, DryExecClient, Environment

# Optional: Set OPENAI_API_KEY in your environment to run live inference
# export OPENAI_API_KEY="sk-..."


class OpenAIModelCaller:
    """Invokes OpenAI ChatCompletion with structured JSON formatting for Action proposals."""

    def __init__(self, api_key: str | None = None, model: str = "gpt-4o-mini"):
        self.api_key = api_key or os.environ.get("OPENAI_API_KEY", "")
        self.model = model
        self._client = None
        if self.api_key:
            try:
                from openai import OpenAI
                self._client = OpenAI(api_key=self.api_key)
            except ImportError:
                self._client = None

    def __call__(self, task: str, history: List[Dict[str, str]]) -> str:
        """Dispatches prompt history to OpenAI or deterministic fallback for CI."""
        if self._client is not None:
            response = self._client.chat.completions.create(
                model=self.model,
                messages=history,  # type: ignore
                response_format={"type": "json_object"},
            )
            return response.choices[0].message.content or "{}"

        # Real SDK fallback for CI / local runs without API key:
        # Step 1: Deliberately attempts invalid target
        # Step 2: Reads error in history and generates valid self-correction
        last_entry = history[-1]["content"] if history else ""
        if "Schema boundary violation" in last_entry or "Execution failed" in last_entry:
            return (
                '{"action_id": "act_retry_02", "target_resource": "user_status", '
                '"mutation_type": "update", "payload": {"status": "verified"}}'
            )
        return (
            '{"action_id": "act_initial_01", "target_resource": "admin_credentials", '
            '"mutation_type": "overwrite", "payload": {"role": "root"}}'
        )


async def main():
    # 1. Define isolated execution boundary
    env = Environment(
        name="onboarding_environment",
        allowed_mutation_targets={"user_status"},
        memory_limit_bytes=1048576,
    )

    # 2. Attach real LLM caller (OpenAI) to the native loop
    llm_caller = OpenAIModelCaller()
    agent = DryExecAgent(environment=env, llm_caller=llm_caller, max_retries=3)

    # 3. Execute autonomous task with ephemeral boundary verification
    result = await agent.run(task="Promote newly registered user to verified status", auto_commit=True)
    print(f"Agent Loop Complete: success={result.success}, trials={result.trials_conducted}, committed={result.committed}")


if __name__ == "__main__":
    asyncio.run(main())
