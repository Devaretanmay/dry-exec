"""dry-exec: Ephemeral execution primitive for deterministic state exploration."""

from dex.agent import Agent, AgentExecutionResult, DryExecAgent
from dex import dry_run, run
from dry_exec.client import DryExecClient
from dry_exec.exceptions import (
    DryExecError,
    IsolationSetupError,
    SchemaViolationError,
    StateDeltaComputationError,
    SyscallBoundaryError,
)
from dry_exec.models import ByteDelta, FsMutation, InterceptedRequest, PageMutation, StateDelta
from dry_exec.observability import DeltaLogger
from dry_exec.schemas import Action, Environment, MockResponse
from dry_exec.telemetry import TelemetryExporter

__all__ = [
    "Agent",
    "dry_run",
    "run",
    "DryExecClient",
    "DryExecAgent",
    "AgentExecutionResult",
    "DeltaLogger",
    "TelemetryExporter",
    "Environment",
    "Action",
    "MockResponse",
    "StateDelta",
    "PageMutation",
    "ByteDelta",
    "FsMutation",
    "InterceptedRequest",
    "DryExecError",
    "SchemaViolationError",
    "IsolationSetupError",
    "SyscallBoundaryError",
    "StateDeltaComputationError",
]

