"""LangChain tool wrapper integration for dry-exec ephemeral exploration."""

import asyncio
from typing import Any, Dict, Optional, Type
from pydantic import BaseModel, Field
from dry_exec import Action, DryExecClient, Environment, StateDelta


class DryExecToolInput(BaseModel):
    """Input schema for dry-exec exploration tool."""

    action_id: str = Field(description="Unique identifier for the action invocation.")
    target_resource: str = Field(
        description="Target resource identifier for state mutation."
    )
    mutation_type: str = Field(
        description="Operation classification (e.g., update, append, set)."
    )
    payload: Dict[str, Any] = Field(
        default_factory=dict, description="Action payload parameters."
    )


class DryExecLangChainTool:
    """Tool wrapper exposing dry-exec kernel boundaries to autonomous execution loops."""

    name: str = "dry_exec_sandbox"
    description: str = (
        "Execute an action ephemerally within an isolated Linux namespace and return "
        "a deterministic StateDelta recording memory, filesystem, and network mutations."
    )
    args_schema: Type[BaseModel] = DryExecToolInput

    def __init__(self, environment: Optional[Environment] = None):
        self.environment = environment or Environment(
            name="langchain_ephemeral_env",
            allowed_mutation_targets={"database", "file", "network"},
            memory_limit_bytes=1048576,
        )
        self.client = DryExecClient()

    def _run(
        self,
        action_id: str,
        target_resource: str,
        mutation_type: str,
        payload: Dict[str, Any],
    ) -> str:
        """Synchronous run interface for LangChain."""
        return asyncio.run(
            self._arun(action_id, target_resource, mutation_type, payload)
        )

    async def _arun(
        self,
        action_id: str,
        target_resource: str,
        mutation_type: str,
        payload: Dict[str, Any],
    ) -> str:
        """Asynchronous execution interface for LangChain agent loops."""
        action = Action(
            action_id=action_id,
            target_resource=target_resource,
            mutation_type=mutation_type,
            payload=payload,
        )
        delta: StateDelta = await self.client.execute_ephemeral_action(
            self.environment, action
        )
        return (
            f"Dry-run executed successfully. StateDelta summary: "
            f"Mutated bytes: {delta.total_bytes_mutated}, "
            f"Memory pages: {len(delta.memory_mutations)}, "
            f"FS mutations: {len(delta.fs_mutations)}, "
            f"Network requests: {len(delta.network_mutations)}."
        )


if __name__ == "__main__":
    tool = DryExecLangChainTool()
    receipt = tool._run(
        action_id="act_lc_001",
        target_resource="database",
        mutation_type="schema_migration",
        payload={"query": "ALTER TABLE users ADD COLUMN verified BOOL;"},
    )
    print("LangChain Tool Execution Output:")
    print(receipt)
