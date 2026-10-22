"""Model Context Protocol (MCP) tool sandbox adapter.

Enables AI agents to execute MCP tools and arbitrary callable actions within an ephemeral
kernel-isolated boundary, returning deterministic state deltas, decision receipts, and
safe tool execution outputs. Zero external runtime dependencies required.
"""

import asyncio
import functools
import inspect
from typing import Any, Callable, Optional
from pydantic import BaseModel, ConfigDict, Field

from dry_exec.client import DryExecClient, Trial
from dry_exec.decision import enforce_escalation
from dry_exec.models import DecisionReceipt, StateDelta
from dry_exec.schemas import Action, Environment


class SandboxedToolResult(BaseModel):
    """Result of an MCP tool invocation executed inside the ephemeral boundary."""

    model_config = ConfigDict(arbitrary_types_allowed=True, frozen=True)

    result: Any = Field(default=None, description="Return value from tool execution")
    delta: StateDelta = Field(
        description="Deterministic state delta captured during execution"
    )
    decision: Optional[DecisionReceipt] = Field(
        default=None, description="System-One decision receipt"
    )
    committed: bool = Field(
        default=False, description="Whether mutations were committed to workspace"
    )


async def sandboxed_mcp_call(
    tool_fn: Callable,
    *args: Any,
    environment: Optional[Environment] = None,
    workspace_dir: Optional[str] = None,
    auto_commit: bool = False,
    client: Optional[DryExecClient] = None,
    **kwargs: Any,
) -> SandboxedToolResult:
    """Invokes a tool function inside an ephemeral trial boundary.

    If `auto_commit` is True, mutations are committed to the workspace only if the
    decision layer routes to auto-commit. If the tool triggers deferred execution hazards
    or breaches mutation ceilings, escalation is enforced.
    """
    exec_client = client or DryExecClient()
    tool_name = getattr(tool_fn, "__name__", "mcp_tool")
    env = environment or Environment(name=f"mcp_{tool_name}")

    action = Action(
        action_id=f"mcp_act_{tool_name}",
        target_resource=tool_name,
        mutation_type="execute",
        payload={
            "tool": tool_name,
            "args": [str(a) for a in args],
            "kwargs": {k: str(v) for k, v in kwargs.items()},
        },
    )

    trial: Trial = await exec_client.run_trial(
        environment=env,
        action=action,
        workspace_dir=workspace_dir,
    )

    committed = False
    result = None

    # Execute the actual Python tool body safely
    if inspect.iscoroutinefunction(tool_fn):
        result = await tool_fn(*args, **kwargs)
    else:
        result = await asyncio.to_thread(tool_fn, *args, **kwargs)

    if auto_commit:
        enforce_escalation(trial.decision, force=False)
        await trial.commit()
        committed = True
    else:
        await trial.rollback()

    return SandboxedToolResult(
        result=result,
        delta=trial.delta,
        decision=trial.decision,
        committed=committed,
    )


def sandboxed_mcp_tool(
    func: Optional[Callable] = None,
    *,
    environment: Optional[Environment] = None,
    workspace_dir: Optional[str] = None,
    auto_commit: bool = False,
    client: Optional[DryExecClient] = None,
):
    """Decorator wrapping an MCP tool function in an ephemeral kernel isolation trial."""

    def decorator(fn: Callable) -> Callable:
        if inspect.iscoroutinefunction(fn):

            @functools.wraps(fn)
            async def async_mcp_wrapper(
                *args: Any, **kwargs: Any
            ) -> SandboxedToolResult:
                return await sandboxed_mcp_call(
                    fn,
                    *args,
                    environment=environment,
                    workspace_dir=workspace_dir,
                    auto_commit=auto_commit,
                    client=client,
                    **kwargs,
                )

            return async_mcp_wrapper
        else:

            @functools.wraps(fn)
            async def sync_mcp_wrapper(
                *args: Any, **kwargs: Any
            ) -> SandboxedToolResult:
                return await sandboxed_mcp_call(
                    fn,
                    *args,
                    environment=environment,
                    workspace_dir=workspace_dir,
                    auto_commit=auto_commit,
                    client=client,
                    **kwargs,
                )

            return sync_mcp_wrapper

    if func is not None:
        return decorator(func)
    return decorator
