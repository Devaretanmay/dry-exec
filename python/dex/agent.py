"""Backward-compatibility shim for dry_exec.agent."""

from dry_exec.agent import Agent, AgentExecutionResult, DryExecAgent  # noqa: F401

__all__ = ["Agent", "AgentExecutionResult", "DryExecAgent"]
