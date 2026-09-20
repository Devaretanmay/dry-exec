"""Verification suite for Loop 9: Structured Telemetry, OTel Tracing, and Native Agent Loop."""

import asyncio
import json
import logging
from unittest.mock import MagicMock
import pytest
from dry_exec import (
    Action,
    DryExecAgent,
    DryExecClient,
    Environment,
    SchemaViolationError,
    StateDelta,
    TelemetryExporter,
)
from dry_exec.models import ByteDelta, PageMutation


class InMemoryLogHandler(logging.Handler):
    """Custom in-memory log collector."""

    def __init__(self):
        super().__init__()
        self.records = []

    def emit(self, record):
        self.records.append(self.format(record))


@pytest.fixture
def memory_logger():
    logger = logging.getLogger("test_dry_exec_telemetry")
    logger.setLevel(logging.INFO)
    handler = InMemoryLogHandler()
    logger.addHandler(handler)
    return logger, handler



def test_structured_json_telemetry_export(memory_logger):
    """Verify TelemetryExporter generates valid JSON structured receipts."""
    logger, handler = memory_logger
    exporter = TelemetryExporter(service_name="test-service", logger=logger, emit_json_logs=True)

    env = Environment(
        name="telemetry_test_env",
        allowed_mutation_targets={"order_status"},
        memory_limit_bytes=2048,
    )
    action = Action(
        action_id="act_telemetry_001",
        target_resource="order_status",
        mutation_type="update",
        payload={"status": "shipped"},
    )
    delta = StateDelta(
        memory_mutations=[
            PageMutation(
                page_index=0,
                page_address=0x1000,
                deltas=[ByteDelta(offset=0, original=b"\x00", mutated=b"\x01")],
            )
        ],
        fs_mutations=[],
        network_mutations=[],
        total_bytes_mutated=1,
        duration_nanos=150_000,
    )

    receipt = exporter.export_delta_receipt(env, action, delta, trial_id=1, committed=True)

    assert receipt["telemetry_type"] == "state_delta_receipt"
    assert receipt["service"] == "test-service"
    assert receipt["committed"] is True
    assert receipt["delta_metrics"]["total_bytes_mutated"] == 1
    assert receipt["delta_metrics"]["duration_ms"] == 0.15

    # Validate JSON log output
    assert len(handler.records) == 1
    logged_json = json.loads(handler.records[0])
    assert logged_json["telemetry_type"] == "state_delta_receipt"
    assert logged_json["environment"]["name"] == "telemetry_test_env"


def test_opentelemetry_span_tracing():
    """Verify OpenTelemetry tracing context manager runs without exception and attaches attributes."""
    exporter = TelemetryExporter(service_name="test-otel-service")

    env = Environment(
        name="otel_test_env",
        allowed_mutation_targets={"counter"},
        memory_limit_bytes=4096,
    )
    action = Action(
        action_id="act_otel_002",
        target_resource="counter",
        mutation_type="increment",
        payload={"step": 1},
    )

    with exporter.trace_ephemeral_action(env, action) as span:
        if span is not None:
            span.set_attribute("custom.execution.marker", "verified")

    # Trace error capture
    with pytest.raises(SchemaViolationError):
        with exporter.trace_ephemeral_action(env, action):
            raise SchemaViolationError("Schema rejection", invalid_target="unauthorized", violation_type="target_error")


@pytest.mark.asyncio
async def test_native_agent_loop_self_correction():
    """Verify DryExecAgent executes multi-step proposal, handles boundary failure, and self-corrects."""
    env = Environment(
        name="agent_test_env",
        allowed_mutation_targets={"balance"},
        memory_limit_bytes=4096,
    )

    # Mock LLM that fails trial 1 then succeeds trial 2
    def mock_llm(task: str, history: list) -> str:
        last_msg = history[-1]["content"] if history else ""
        if "Schema boundary violation" in last_msg or "Execution failed" in last_msg:
            return json.dumps({
                "action_id": "act_retry_valid",
                "target_resource": "balance",
                "mutation_type": "update",
                "payload": {"amount": 500},
            })
        return json.dumps({
            "action_id": "act_fail_invalid",
            "target_resource": "unauthorized_field",
            "mutation_type": "update",
            "payload": {"amount": 500},
        })

    # Mock client execution returning a valid delta
    mock_client = MagicMock()
    async def mock_execute(environment, action, **kwargs):
        environment.validate_action(action)
        return StateDelta(
            memory_mutations=[],
            fs_mutations=[],
            network_mutations=[],
            total_bytes_mutated=8,
            duration_nanos=200_000,
        )
    mock_client.execute_ephemeral_action = mock_execute

    agent = DryExecAgent(environment=env, llm_caller=mock_llm, client=mock_client, max_retries=3)
    result = await agent.run(task="Adjust account balance", auto_commit=True)

    assert result.success is True
    assert result.trials_conducted == 2
    assert result.committed is True
    assert len(result.error_history) == 1
    assert "Schema boundary violation" in result.error_history[0]
