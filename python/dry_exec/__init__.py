"""dry-exec: Ephemeral execution primitive for deterministic state exploration."""

import asyncio
import ast
import concurrent.futures
import functools
import inspect
import os
import textwrap
import tempfile
import sys
import uuid
from typing import Callable, Optional, Set, Union

from dry_exec.agent import Agent, AgentExecutionResult, DryExecAgent
from dry_exec.client import DryExecClient, Trial
from dry_exec.decision import (
    enforce_escalation,
    is_escalated,
    parse_decision,
)
from dry_exec.exceptions import (
    DecisionEscalationError,
    DryExecError,
    IsolationSetupError,
    SchemaViolationError,
    StateDeltaComputationError,
    SyscallBoundaryError,
)
from dry_exec.mcp import (
    SandboxedToolResult,
    sandboxed_mcp_call,
    sandboxed_mcp_tool,
)
from dry_exec.models import (
    ByteDelta,
    Choice,
    DecisionReceipt,
    FsMutation,
    InterceptedRequest,
    Noul,
    PageMutation,
    StateDelta,
)
from dry_exec.schemas import Action, Environment, MockResponse, SecurityLevel


def _exec_coro_safely(coro):
    """Executes a coroutine safely whether inside or outside an active event loop."""
    try:
        loop = asyncio.get_running_loop()
    except RuntimeError:
        loop = None

    if loop is not None and loop.is_running():
        with concurrent.futures.ThreadPoolExecutor(max_workers=1) as executor:
            future = executor.submit(asyncio.run, coro)
            return future.result()
    return asyncio.run(coro)


def _callable_command(fn: Callable, args, kwargs):
    """Create a short-lived Python worker script for isolated execution."""
    source = textwrap.dedent(inspect.getsource(fn))
    module = ast.parse(source)
    definition = next(
        node
        for node in module.body
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
    )
    definition.decorator_list = []
    source = ast.unparse(definition)
    invocation = f"{fn.__name__}(*{args!r}, **{kwargs!r})"
    if inspect.iscoroutinefunction(fn):
        invocation = f"asyncio.run({invocation})"
    script = f"import asyncio\n{source}\n\n{invocation}\n"
    handle = tempfile.NamedTemporaryFile(
        mode="w", suffix=".py", prefix="dry_exec_callable_", delete=False
    )
    try:
        handle.write(script)
        path = handle.name
    finally:
        handle.close()
    return [sys.executable, path], path


async def _execute_with_workspace(
    client: "DryExecClient",
    environment: Environment,
    action: Action,
    workspace_dir: Optional[str],
    commit: bool,
) -> StateDelta:
    """Execute against a workspace trial and apply the captured delta exactly once.

    A commit requires an explicit workspace target. The action never re-executes on the host;
    only the captured filesystem delta is applied through Trial's transactional journal.
    """
    if not commit and workspace_dir is None:
        delta = await client.execute_ephemeral_action(environment, action)
        enforce_escalation(delta.decision, force=commit)
        return delta

    if workspace_dir is None:
        raise ValueError(
            "commit=True requires an explicit workspace_dir; refusing to apply mutations "
            "to the process working directory."
        )

    trial = await client.run_trial(
        environment,
        action,
        workspace_dir=workspace_dir,
    )
    try:
        enforce_escalation(trial.decision, force=commit)
        if commit:
            await trial.commit(force=commit)
        else:
            await trial.rollback()
    except BaseException:
        if not trial.is_committed and not trial.is_rolled_back:
            await trial.rollback()
        raise
    return trial.delta


def dry_run(
    func: Optional[Callable] = None,
    *,
    environment: Optional[Environment] = None,
    allowed_targets: Optional[Set[str]] = None,
    memory_limit_bytes: int = 64 * 1024 * 1024,
    commit: bool = False,
    workspace_dir: Optional[str] = None,
    client: Optional[DryExecClient] = None,
):
    """Decorator wrapping function execution within an ephemeral kernel isolation boundary.

    Returns the computed StateDelta showing exact byte/inode/network mutations.
    Host state remains untouched unless commit=True.
    """

    def decorator(fn: Callable):
        target_name = getattr(fn, "__name__", "ephemeral_target")
        target_env = environment or Environment(
            name=f"sandbox_{target_name}",
            allowed_mutation_targets=allowed_targets or {"*"},
            memory_limit_bytes=memory_limit_bytes,
        )
        exec_client = client or DryExecClient()

        if inspect.iscoroutinefunction(fn):

            @functools.wraps(fn)
            async def async_wrapper(*args, **kwargs) -> StateDelta:
                command, script_path = _callable_command(fn, args, kwargs)
                action = Action(
                    action_id=f"act_{target_name}_{uuid.uuid4().hex[:6]}",
                    target_resource=target_name,
                    mutation_type="execute",
                    payload={
                        "args": [str(a) for a in args],
                        "kwargs": {k: str(v) for k, v in kwargs.items()},
                        "command": command,
                    },
                )
                try:
                    return await _execute_with_workspace(
                        exec_client,
                        target_env,
                        action,
                        workspace_dir,
                        commit,
                    )
                finally:
                    try:
                        os.unlink(script_path)
                    except FileNotFoundError:
                        pass

            return async_wrapper
        else:

            @functools.wraps(fn)
            def sync_wrapper(*args, **kwargs) -> StateDelta:
                command, script_path = _callable_command(fn, args, kwargs)
                action = Action(
                    action_id=f"act_{target_name}_{uuid.uuid4().hex[:6]}",
                    target_resource=target_name,
                    mutation_type="execute",
                    payload={
                        "args": [str(a) for a in args],
                        "kwargs": {k: str(v) for k, v in kwargs.items()},
                        "command": command,
                    },
                )
                try:
                    return _exec_coro_safely(
                        _execute_with_workspace(
                            exec_client,
                            target_env,
                            action,
                            workspace_dir,
                            commit,
                        )
                    )
                finally:
                    try:
                        os.unlink(script_path)
                    except FileNotFoundError:
                        pass

            return sync_wrapper

    if func is not None:
        return decorator(func)
    return decorator


def run(
    target: Union[Callable, str],
    *args,
    environment: Optional[Environment] = None,
    commit: bool = False,
    workspace_dir: Optional[str] = None,
    client: Optional[DryExecClient] = None,
    **kwargs,
) -> StateDelta:
    """Executes a callable or shell command within an ephemeral isolation boundary."""
    exec_client = client or DryExecClient()

    if isinstance(target, str):
        target_env = environment or Environment(
            name="cli_ephemeral_sandbox",
            allowed_mutation_targets={"*"},
        )
        action = Action(
            action_id=f"act_cmd_{uuid.uuid4().hex[:6]}",
            target_resource="shell_command",
            mutation_type="execute",
            payload={"command": target},
        )
        return _exec_coro_safely(
            _execute_with_workspace(
                exec_client,
                target_env,
                action,
                workspace_dir,
                commit,
            )
        )
    elif callable(target):
        wrapper = dry_run(
            target,
            environment=environment,
            commit=commit,
            workspace_dir=workspace_dir,
            client=exec_client,
        )
        return wrapper(*args, **kwargs)
    else:
        raise TypeError(
            f"Expected callable or shell command string, got {type(target).__name__}"
        )


__all__ = [
    "Agent",
    "AgentExecutionResult",
    "DryExecAgent",
    "dry_run",
    "run",
    "DryExecClient",
    "Trial",
    "sandboxed_mcp_tool",
    "sandboxed_mcp_call",
    "SandboxedToolResult",
    "Environment",
    "SecurityLevel",
    "Action",
    "MockResponse",
    "StateDelta",
    "PageMutation",
    "ByteDelta",
    "FsMutation",
    "InterceptedRequest",
    "Choice",
    "Noul",
    "DecisionReceipt",
    "is_escalated",
    "parse_decision",
    "enforce_escalation",
    "DryExecError",
    "SchemaViolationError",
    "SyscallBoundaryError",
    "IsolationSetupError",
    "StateDeltaComputationError",
    "DecisionEscalationError",
]
