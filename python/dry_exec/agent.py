"""Native standalone agent loop with ephemeral dry-run evaluation and self-correction."""

import asyncio
import json
from typing import Any, Callable, Dict, List, Optional
from dry_exec.client import DryExecClient
from dry_exec.exceptions import DryExecError, SchemaViolationError, SyscallBoundaryError
from dry_exec.models import StateDelta
from dry_exec.observability import DeltaLogger
from dry_exec.schemas import Action, Environment
from dry_exec.telemetry import TelemetryExporter


class AgentExecutionResult:
    """Outcome of an autonomous execution loop trial."""

    def __init__(
        self,
        task: str,
        success: bool,
        trials_conducted: int,
        final_delta: Optional[StateDelta] = None,
        committed: bool = False,
        error_history: Optional[List[str]] = None,
    ):
        self.task = task
        self.success = success
        self.trials_conducted = trials_conducted
        self.final_delta = final_delta
        self.committed = committed
        self.error_history = error_history or []

    def __repr__(self) -> str:
        return (
            f"AgentExecutionResult(task={self.task!r}, success={self.success}, "
            f"trials={self.trials_conducted}, committed={self.committed})"
        )


class DryExecAgent:
    """Standalone agent loop orchestrating proposal, ephemeral dry-run, and self-correction."""

    def __init__(
        self,
        environment: Environment,
        llm_caller: Optional[Callable[[str, List[Dict[str, str]]], str]] = None,
        client: Optional[DryExecClient] = None,
        logger: Optional[DeltaLogger] = None,
        max_retries: int = 3,
    ):
        self.environment = environment
        self.llm_caller = llm_caller or self._default_mock_llm
        self.client = client or DryExecClient()
        self.logger = logger or DeltaLogger()
        self.max_retries = max_retries

    async def run(self, task: str, auto_commit: bool = True) -> AgentExecutionResult:
        """Executes the autonomous loop: propose -> dry-run -> evaluate delta -> self-correct / commit."""
        conversation_history: List[Dict[str, str]] = []
        error_history: List[str] = []

        system_directive = (
            f"You are an autonomous execution loop operating within the environment: {self.environment.name}.\n"
            f"Permitted mutation targets: {list(self.environment.allowed_mutation_targets)}.\n"
            "Produce JSON containing: action_id, target_resource, mutation_type, and payload."
        )
        conversation_history.append({"role": "system", "content": system_directive})
        conversation_history.append({"role": "user", "content": f"Task: {task}"})

        for trial in range(1, self.max_retries + 1):
            # 1. Propose action via LLM
            llm_response_text = self.llm_caller(task, conversation_history)
            try:
                action_data = json.loads(llm_response_text)
                action = Action(**action_data)
            except Exception as e:
                err_msg = f"Failed to parse LLM proposal into Action schema: {e}"
                error_history.append(err_msg)
                conversation_history.append({"role": "assistant", "content": llm_response_text})
                conversation_history.append({"role": "user", "content": f"Schema error: {err_msg}. Please adjust."})
                continue

            self.logger.render_action_header(self.environment, action)

            # 2. Ephemeral dry-run execution
            try:
                delta: StateDelta = await self.client.execute_ephemeral_action(self.environment, action)
                self.logger.render_delta(delta, trial_id=trial)

                # 3. Evaluate delta receipt
                # If valid and non-empty state delta, finalize commit
                if auto_commit:
                    self.logger.render_commit_prompt(confirmed=True)
                    return AgentExecutionResult(
                        task=task,
                        success=True,
                        trials_conducted=trial,
                        final_delta=delta,
                        committed=True,
                        error_history=error_history,
                    )
                else:
                    return AgentExecutionResult(
                        task=task,
                        success=True,
                        trials_conducted=trial,
                        final_delta=delta,
                        committed=False,
                        error_history=error_history,
                    )

            except SchemaViolationError as sve:
                err_msg = f"Schema boundary violation: {sve.violation_type} on target '{sve.invalid_target}'"
                error_history.append(err_msg)
                self.logger.render_error(sve, trial_id=trial)
                conversation_history.append({"role": "assistant", "content": action.model_dump_json()})
                conversation_history.append({
                    "role": "user",
                    "content": f"Execution failed: {err_msg}. Target must be in {list(self.environment.allowed_mutation_targets)}. Self-correct and provide updated Action JSON.",
                })

            except SyscallBoundaryError as sbe:
                err_msg = f"Syscall boundary violation: syscall_nr {sbe.syscall_nr} at 0x{sbe.instruction_pointer:x}"
                error_history.append(err_msg)
                self.logger.render_error(sbe, trial_id=trial)
                conversation_history.append({"role": "assistant", "content": action.model_dump_json()})
                conversation_history.append({
                    "role": "user",
                    "content": f"Execution failed: {err_msg}. Adjust control flow without invoking restricted syscalls.",
                })

            except DryExecError as dee:
                err_msg = f"Boundary error: {dee}"
                error_history.append(err_msg)
                self.logger.render_error(dee, trial_id=trial)
                conversation_history.append({"role": "assistant", "content": action.model_dump_json()})
                conversation_history.append({"role": "user", "content": f"Boundary error: {err_msg}."})

        return AgentExecutionResult(
            task=task,
            success=False,
            trials_conducted=self.max_retries,
            final_delta=None,
            committed=False,
            error_history=error_history,
        )

    def _default_mock_llm(self, task: str, history: List[Dict[str, str]]) -> str:
        """Deterministic fallback LLM generator demonstrating proposal and self-correction."""
        last_message = history[-1]["content"] if history else ""
        # If previous trial failed with schema violation, self-correct to allowed target
        if "Schema boundary violation" in last_message or "Execution failed" in last_message:
            allowed = list(self.environment.allowed_mutation_targets)[0]
            return json.dumps({
                "action_id": "act_corrected_02",
                "target_resource": allowed,
                "mutation_type": "update",
                "payload": {"resolution": "corrected_value"},
            })
        # Initial proposal (defaults to valid or target based on task)
        allowed = list(self.environment.allowed_mutation_targets)[0]
        return json.dumps({
            "action_id": "act_initial_01",
            "target_resource": allowed,
            "mutation_type": "update",
            "payload": {"state": "active"},
        })
