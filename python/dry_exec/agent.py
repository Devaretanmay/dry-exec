"""Streamlined autonomous execution loop with ephemeral dry-run evaluation and self-correction."""

import json
import os
from typing import Any, Callable, Dict, List, Optional
from dry_exec.client import DryExecClient
from dry_exec.decision import is_escalated
from dry_exec.exceptions import (
    DecisionEscalationError,
    DryExecError,
    SchemaViolationError,
    SyscallBoundaryError,
)
from dry_exec.models import StateDelta
from dry_exec.observability import DeltaLogger
from dry_exec.schemas import Action, Environment


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


class Agent:
    """Autonomous execution loop orchestrating proposal, ephemeral dry-run, and self-correction."""

    def __init__(
        self,
        task: Optional[str] = None,
        llm: Optional[Any] = None,
        llm_caller: Optional[Any] = None,
        environment: Optional[Environment] = None,
        client: Optional[DryExecClient] = None,
        logger: Optional[DeltaLogger] = None,
        max_retries: int = 3,
    ):
        self.task = task
        self.environment = environment or Environment(
            name="ephemeral_agent_sandbox",
            allowed_mutation_targets={"*"},
            memory_limit_bytes=64 * 1024 * 1024,
        )
        self.client = client or DryExecClient()
        self.logger = logger or DeltaLogger()
        self.max_retries = max_retries
        self.llm = llm or llm_caller or self._resolve_default_llm()

    def _resolve_default_llm(self) -> Callable[[str, List[Dict[str, str]]], str]:
        """Resolves OpenAI SDK caller if configured, or deterministic CI caller."""
        api_key = os.environ.get("OPENAI_API_KEY")
        if api_key:
            try:
                import openai

                client = openai.OpenAI(api_key=api_key)

                def _openai_caller(task: str, history: List[Dict[str, str]]) -> str:
                    resp = client.chat.completions.create(
                        model="gpt-4o-mini",
                        messages=history,
                        temperature=0.0,
                    )
                    return resp.choices[0].message.content or "{}"

                return _openai_caller
            except ImportError:
                pass
        return self._default_mock_llm

    async def run(
        self,
        task: Optional[str] = None,
        auto_commit: bool = True,
        force: bool = False,
    ) -> AgentExecutionResult:
        """Executes the autonomous loop: propose -> dry-run -> evaluate receipt -> self-correct / commit.

        A receipt routed to escalation halts the loop and raises DecisionEscalationError unless
        force=True, which is the programmatic equivalent of the CLI's explicit --commit.
        """
        active_task = task or self.task
        if not active_task:
            raise ValueError(
                "Task must be specified either at initialization or in run()."
            )

        conversation_history: List[Dict[str, str]] = []
        error_history: List[str] = []

        allowed_desc = (
            "any"
            if "*" in self.environment.allowed_mutation_targets
            else list(self.environment.allowed_mutation_targets)
        )
        system_directive = (
            f"You are an autonomous execution loop operating within the environment: {self.environment.name}.\n"
            f"Permitted mutation targets: {allowed_desc}.\n"
            "Produce JSON containing: action_id, target_resource, mutation_type, and payload."
        )
        conversation_history.append({"role": "system", "content": system_directive})
        conversation_history.append({"role": "user", "content": f"Task: {active_task}"})

        for trial in range(1, self.max_retries + 1):
            llm_response_text = self.llm(active_task, conversation_history)
            try:
                action_data = json.loads(llm_response_text)
                action = Action(**action_data)
            except Exception as e:
                err_msg = f"Failed to parse proposal into Action schema: {e}"
                error_history.append(err_msg)
                conversation_history.append(
                    {"role": "assistant", "content": llm_response_text}
                )
                conversation_history.append(
                    {
                        "role": "user",
                        "content": f"Schema error: {err_msg}. Please adjust.",
                    }
                )
                continue

            self.logger.render_action_header(self.environment, action)

            try:
                delta: StateDelta = await self.client.execute_ephemeral_action(
                    self.environment, action
                )
                self.logger.render_delta(delta, trial_id=trial)
                self.logger.render_decision(delta.decision)

                # System-One routing: escalation halts the loop unless the commit is forced
                if is_escalated(delta.decision) and not force:
                    raise DecisionEscalationError(
                        message=delta.decision.reason,
                        risk_score=delta.decision.risk_score,
                        reason=delta.decision.reason,
                        choice=delta.decision.choice.value,
                    )

                if auto_commit:
                    self.logger.render_commit_prompt(confirmed=True)
                    return AgentExecutionResult(
                        task=active_task,
                        success=True,
                        trials_conducted=trial,
                        final_delta=delta,
                        committed=True,
                        error_history=error_history,
                    )
                else:
                    return AgentExecutionResult(
                        task=active_task,
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
                conversation_history.append(
                    {"role": "assistant", "content": action.model_dump_json()}
                )
                conversation_history.append(
                    {
                        "role": "user",
                        "content": f"Execution failed: {err_msg}. Permitted targets: {allowed_desc}. Self-correct and provide updated Action JSON.",
                    }
                )

            except SyscallBoundaryError as sbe:
                err_msg = f"Syscall boundary violation: syscall_nr {sbe.syscall_nr} at 0x{sbe.instruction_pointer:x}"
                error_history.append(err_msg)
                self.logger.render_error(sbe, trial_id=trial)
                conversation_history.append(
                    {"role": "assistant", "content": action.model_dump_json()}
                )
                conversation_history.append(
                    {
                        "role": "user",
                        "content": f"Execution failed: {err_msg}. Adjust control flow without invoking restricted syscalls.",
                    }
                )

            except DecisionEscalationError:
                # Escalation is terminal for the loop: it is not a boundary breach that
                # self-correction can resolve, so it must not be absorbed as a boundary error.
                raise

            except DryExecError as dee:
                err_msg = f"Boundary error: {dee}"
                error_history.append(err_msg)
                self.logger.render_error(dee, trial_id=trial)
                conversation_history.append(
                    {"role": "assistant", "content": action.model_dump_json()}
                )
                conversation_history.append(
                    {"role": "user", "content": f"Boundary error: {err_msg}."}
                )

        return AgentExecutionResult(
            task=active_task,
            success=False,
            trials_conducted=self.max_retries,
            final_delta=None,
            committed=False,
            error_history=error_history,
        )

    def _default_mock_llm(self, task: str, history: List[Dict[str, str]]) -> str:
        """Deterministic fallback LLM generator demonstrating proposal and self-correction."""
        last_message = history[-1]["content"] if history else ""
        if (
            "Schema boundary violation" in last_message
            or "Execution failed" in last_message
        ):
            allowed = (
                "default_resource"
                if "*" in self.environment.allowed_mutation_targets
                else list(self.environment.allowed_mutation_targets)[0]
            )
            return json.dumps(
                {
                    "action_id": "act_corrected_02",
                    "target_resource": allowed,
                    "mutation_type": "update",
                    "payload": {"resolution": "corrected_value"},
                }
            )

        allowed = (
            "default_resource"
            if "*" in self.environment.allowed_mutation_targets
            else list(self.environment.allowed_mutation_targets)[0]
        )
        return json.dumps(
            {
                "action_id": "act_initial_01",
                "target_resource": allowed,
                "mutation_type": "update",
                "payload": {"state": "active"},
            }
        )


# Backward-compatible alias
DryExecAgent = Agent
