"""dry-exec: Ephemeral execution primitive for deterministic state exploration."""

import asyncio
import concurrent.futures
import functools
import inspect
import uuid
from typing import Any, Callable, Dict, List, Optional, Set, Union

from dry_exec.agent import Agent, AgentExecutionResult, DryExecAgent
from dry_exec.client import DryExecClient
from dry_exec.exceptions import (
    DryExecError,
    IsolationSetupError,
    SchemaViolationError,
    StateDeltaComputationError,
    SyscallBoundaryError,
)
from dry_exec.models import (
    ByteDelta,
    FsMutation,
    InterceptedRequest,
    PageMutation,
    StateDelta,
)
from dry_exec.observability import DeltaLogger
from dry_exec.schemas import Action, Environment, MockResponse
from dry_exec.telemetry import TelemetryExporter


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


def dry_run(
    func: Optional[Callable] = None,
    *,
    environment: Optional[Environment] = None,
    allowed_targets: Optional[Set[str]] = None,
    memory_limit_bytes: int = 64 * 1024 * 1024,
    commit: bool = False,
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
                action = Action(
                    action_id=f"act_{target_name}_{uuid.uuid4().hex[:6]}",
                    target_resource=target_name,
                    mutation_type="execute",
                    payload={
                        "args": [str(a) for a in args],
                        "kwargs": {k: str(v) for k, v in kwargs.items()},
                    },
                )
                delta = await exec_client.execute_ephemeral_action(target_env, action)
                if commit:
                    await fn(*args, **kwargs)
                return delta

            return async_wrapper
        else:
            @functools.wraps(fn)
            def sync_wrapper(*args, **kwargs) -> StateDelta:
                action = Action(
                    action_id=f"act_{target_name}_{uuid.uuid4().hex[:6]}",
                    target_resource=target_name,
                    mutation_type="execute",
                    payload={
                        "args": [str(a) for a in args],
                        "kwargs": {k: str(v) for k, v in kwargs.items()},
                    },
                )
                delta = _exec_coro_safely(exec_client.execute_ephemeral_action(target_env, action))
                if commit:
                    fn(*args, **kwargs)
                return delta

            return sync_wrapper

    if func is not None:
        return decorator(func)
    return decorator


def run(
    target: Union[Callable, str],
    *args,
    environment: Optional[Environment] = None,
    commit: bool = False,
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
        delta = _exec_coro_safely(exec_client.execute_ephemeral_action(target_env, action))
        if commit:
            import subprocess
            subprocess.run(target, shell=True, check=True)
        return delta
    elif callable(target):
        wrapper = dry_run(target, environment=environment, commit=commit, client=exec_client)
        return wrapper(*args, **kwargs)
    else:
        raise TypeError(f"Expected callable or shell command string, got {type(target).__name__}")


__all__ = [
    "Agent",
    "AgentExecutionResult",
    "DryExecAgent",
    "dry_run",
    "run",
    "DryExecClient",
    "Environment",
    "Action",
    "MockResponse",
    "StateDelta",
    "PageMutation",
    "ByteDelta",
    "FsMutation",
    "InterceptedRequest",
    "DeltaLogger",
    "TelemetryExporter",
    "DryExecError",
    "SchemaViolationError",
    "SyscallBoundaryError",
    "IsolationSetupError",
    "StateDeltaComputationError",
]

