"""Model Context Protocol (MCP) tool sandbox adapter.

Enables AI agents to execute MCP tools and arbitrary callable actions within an ephemeral
kernel-isolated boundary, returning deterministic state deltas, decision receipts, and
safe tool execution outputs. Zero external runtime dependencies required.
"""

import functools
import ast
import inspect
import json
import os
from pathlib import Path
import tempfile
import sys
import stat
import textwrap
from typing import Any, Callable, Optional
import uuid
from pydantic import BaseModel, ConfigDict, Field

from dry_exec.client import DryExecClient
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


def _prepare_tool_script(
    fn: Callable, args: tuple, kwargs: dict, result_file_name: str
) -> str:
    """Serializes tool function into an isolated Python runner script."""
    source = textwrap.dedent(inspect.getsource(fn))
    module = ast.parse(source)
    definition = next(
        node
        for node in module.body
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
    )
    definition.decorator_list = []
    cleaned_source = ast.unparse(definition)

    return f"""import sys, json, asyncio, inspect, os
from pathlib import Path
{cleaned_source}

async def _dex_entry():
    if inspect.iscoroutinefunction({fn.__name__}):
        res = await {fn.__name__}(*{args!r}, **{kwargs!r})
    else:
        res = {fn.__name__}(*{args!r}, **{kwargs!r})
    try:
        data = json.dumps(res)
    except Exception:
        data = json.dumps(str(res))
    with open({result_file_name!r}, "w") as f:
        f.write(data)

if __name__ == "__main__":
    asyncio.run(_dex_entry())
"""


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

    The callable executes within the kernel boundary; all filesystem and network mutations
    are captured in the state delta. If `auto_commit` is True, mutations are applied to the
    target workspace only if the decision layer routes to auto-commit.
    """
    exec_client = client or DryExecClient()
    tool_name = getattr(tool_fn, "__name__", "mcp_tool")
    env = environment or Environment(name=f"mcp_{tool_name}")

    result_file_name = f".dex_mcp_out_{uuid.uuid4().hex[:8]}.json"
    command = None
    script_path = None

    try:
        script_content = _prepare_tool_script(tool_fn, args, kwargs, result_file_name)
        with tempfile.NamedTemporaryFile(
            mode="w", suffix=".py", prefix="dry_exec_mcp_", delete=False
        ) as handle:
            handle.write(script_content)
            script_path = handle.name
        command = [sys.executable, script_path]
    except (TypeError, OSError) as error:
        # Fallback if function source cannot be extracted (built-in or C-extension)
        raw_cmd = kwargs.get("command")
        if isinstance(raw_cmd, list):
            command = raw_cmd
        elif isinstance(raw_cmd, str) and raw_cmd.strip():
            command = ["/bin/sh", "-c", raw_cmd]
        else:
            raise TypeError(
                "Tool source is unavailable; supply an explicit command"
            ) from error

    action = Action(
        action_id=f"mcp_act_{tool_name}_{uuid.uuid4().hex[:6]}",
        target_resource=tool_name,
        mutation_type="execute",
        payload={
            "tool": tool_name,
            "args": [str(a) for a in args],
            "kwargs": {k: str(v) for k, v in kwargs.items()},
            "command": command,
        },
    )

    trial = None
    try:
        trial = await exec_client.run_trial(
            environment=env,
            action=action,
            workspace_dir=workspace_dir,
        )

        result = None
        if trial._scratch_dir:
            res_path = Path(trial._scratch_dir) / result_file_name
            # Open without following links or blocking on tool-created FIFOs, then check the
            # opened inode so a background tool cannot race a separate pathname check.
            try:
                result_fd = os.open(
                    res_path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK
                )
            except FileNotFoundError:
                result_fd = None
            if result_fd is not None:
                with os.fdopen(result_fd, "r") as handle:
                    if not stat.S_ISREG(os.fstat(handle.fileno()).st_mode):
                        raise ValueError("Tool result must be a regular file")
                    content = handle.read()
                try:
                    result = json.loads(content)
                except ValueError:
                    result = content
                res_path.unlink()

        committed = False
        if auto_commit and workspace_dir is not None:
            enforce_escalation(trial.decision, force=False)
            await trial.commit()
            committed = trial.is_committed

        return SandboxedToolResult(
            result=result,
            delta=trial.delta,
            decision=trial.decision,
            committed=committed,
        )
    finally:
        if trial is not None and not trial.is_committed:
            await trial.rollback()
        if script_path:
            Path(script_path).unlink(missing_ok=True)


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
