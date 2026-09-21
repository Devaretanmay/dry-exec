"""Verification suite for Loop 13: The System-One Decision Layer.

Covers the Python-side mapping and escalation routing, the CLI and autonomous execution loop
gates, and end-to-end routing produced by the Rust decision layer across the FFI boundary.
"""

import json
from pathlib import Path
import re
import sys

import pytest
from pydantic import ValidationError

from dry_exec.decision import enforce_escalation, is_escalated, parse_decision
from dry_exec.exceptions import DecisionEscalationError
from dry_exec.models import Choice, DecisionReceipt, Noul, StateDelta
from dry_exec.schemas import Action, Environment, MockResponse

REQUIRES_KERNEL_BOUNDARY = pytest.mark.skipif(
    sys.platform != "linux",
    reason="Requires the Linux kernel execution boundary and Rust FFI primitive",
)


def _receipt(
    choice: Choice = Choice.ALLOWED,
    risk_score: float = 0.1,
    noul_trigger: Noul = Noul.AUTO_COMMIT,
    reason: str = "Risk score 0.10 within calibrated threshold 0.50",
) -> DecisionReceipt:
    """Deterministic receipt fixture for routing assertions."""
    return DecisionReceipt(
        choice=choice,
        risk_score=risk_score,
        noul_trigger=noul_trigger,
        reason=reason,
    )


def _plain(output: str) -> str:
    """Collapse Rich panel borders and line wrapping so assertions read the rendered text."""
    return " ".join(re.sub(r"[│╭╮╰╯─┬┴┼]", " ", output).split())


def _escalated_delta() -> StateDelta:
    """State delta whose receipt routes to escalation."""
    return StateDelta(
        total_bytes_mutated=16,
        duration_nanos=42_000,
        decision=_receipt(
            choice=Choice.VIOLATED,
            risk_score=0.85,
            noul_trigger=Noul.ESCALATE,
            reason="Mutation volume 16 bytes exceeds calibrated ceiling 8 bytes",
        ),
    )


def test_decision_receipt_maps_ffi_payload():
    """Verify the routed FFI payload maps onto the type-safe receipt model."""
    receipt = parse_decision(
        {
            "choice": "violated",
            "risk_score": 0.85,
            "noul_trigger": "escalate",
            "reason": "Mutation volume 16 bytes exceeds calibrated ceiling 8 bytes",
        }
    )

    assert receipt is not None
    assert receipt.choice is Choice.VIOLATED
    assert receipt.noul_trigger is Noul.ESCALATE
    assert receipt.risk_score == 0.85


def test_parse_decision_absent_payload_is_none():
    """Verify an unset decision payload does not fabricate a receipt."""
    assert parse_decision(None) is None
    assert parse_decision({}) is None


def test_decision_receipt_rejects_uncalibrated_score():
    """Verify the receipt model enforces the calibrated score range."""
    with pytest.raises(ValidationError):
        DecisionReceipt(
            choice=Choice.ALLOWED,
            risk_score=1.4,
            noul_trigger=Noul.AUTO_COMMIT,
            reason="out of range",
        )


def test_is_escalated_requires_a_routed_receipt():
    """Verify escalation state is read from the receipt and absent otherwise."""
    assert is_escalated(None) is False
    assert is_escalated(_receipt()) is False
    assert is_escalated(_receipt(noul_trigger=Noul.ESCALATE)) is True


def test_enforce_escalation_halts_and_carries_telemetry():
    """Verify escalation halts control flow and surfaces the calibrated metrics."""
    with pytest.raises(DecisionEscalationError) as exc_info:
        enforce_escalation(
            _receipt(
                choice=Choice.VIOLATED,
                risk_score=0.85,
                noul_trigger=Noul.ESCALATE,
                reason="Risk score 0.85 exceeds calibrated threshold 0.50",
            )
        )

    assert exc_info.value.risk_score == 0.85
    assert exc_info.value.choice == "violated"
    assert "exceeds calibrated threshold" in exc_info.value.reason


def test_enforce_escalation_clears_on_force_or_auto_commit():
    """Verify a forced commit and an auto-commit receipt both proceed."""
    enforce_escalation(_receipt(noul_trigger=Noul.ESCALATE), force=True)
    enforce_escalation(_receipt())
    enforce_escalation(None)


def test_environment_exposes_calibrated_thresholds():
    """Verify the calibrated thresholds are schema-validated and carry deterministic defaults."""
    environment = Environment(name="calibration_env")

    assert environment.max_mutated_bytes == 1_048_576
    assert environment.max_network_calls == 16
    assert environment.max_risk_threshold == 0.5

    with pytest.raises(ValidationError):
        Environment(name="invalid_env", max_mutated_bytes=0)


def _cli_boundary_files(tmp_path: Path) -> tuple:
    """Environment and action configuration files for CLI invocation."""
    env_file = tmp_path / "env.json"
    env_file.write_text(
        json.dumps(
            {
                "name": "decision_cli_env",
                "allowed_mutation_targets": ["balance"],
                "memory_limit_bytes": 1024 * 1024,
            }
        ),
        encoding="utf-8",
    )
    action_file = tmp_path / "action.json"
    action_file.write_text(
        json.dumps(
            {
                "action_id": "cli_act_escalate",
                "target_resource": "balance",
                "mutation_type": "credit",
                "payload": {"amount": 100},
            }
        ),
        encoding="utf-8",
    )
    return env_file, action_file


def _patch_cli_client(monkeypatch, delta: StateDelta) -> None:
    """Route CLI execution through a deterministic state delta."""
    import dry_exec.cli

    async def mock_exec(*args, **kwargs):
        return delta

    monkeypatch.setattr(
        dry_exec.cli.DryExecClient, "execute_ephemeral_action", mock_exec
    )


def test_cli_blocks_commit_on_escalation(tmp_path, monkeypatch):
    """Verify escalation halts the CLI with exit code 4 and requires an explicit flag."""
    pytest.importorskip("typer")
    from typer.testing import CliRunner
    from dry_exec.cli import app

    env_file, action_file = _cli_boundary_files(tmp_path)
    _patch_cli_client(monkeypatch, _escalated_delta())

    result = CliRunner().invoke(
        app,
        ["run", "--config", str(env_file), "--action", str(action_file)],
    )

    assert result.exit_code == 4
    assert "System-One Decision: Escalation Required" in result.stdout
    assert "Escalation required. Re-run with --commit to force." in _plain(
        result.stdout
    )


def test_cli_forced_commit_clears_escalation(tmp_path, monkeypatch):
    """Verify an explicit --commit forces past escalation."""
    pytest.importorskip("typer")
    from typer.testing import CliRunner
    from dry_exec.cli import app

    env_file, action_file = _cli_boundary_files(tmp_path)
    _patch_cli_client(monkeypatch, _escalated_delta())

    result = CliRunner().invoke(
        app,
        [
            "run",
            "--config",
            str(env_file),
            "--action",
            str(action_file),
            "--commit",
        ],
    )

    assert result.exit_code == 0


def test_cli_auto_commits_calibrated_delta(tmp_path, monkeypatch):
    """Verify a receipt routed to auto-commit needs no escalation flag."""
    pytest.importorskip("typer")
    from typer.testing import CliRunner
    from dry_exec.cli import app

    env_file, action_file = _cli_boundary_files(tmp_path)
    calibrated = StateDelta(
        total_bytes_mutated=4,
        duration_nanos=11_000,
        decision=_receipt(),
    )
    _patch_cli_client(monkeypatch, calibrated)

    # Declining the interactive commit checkpoint leaves host state untouched with exit code 0.
    result = CliRunner().invoke(
        app,
        ["run", "--config", str(env_file), "--action", str(action_file)],
        input="n\n",
    )

    assert result.exit_code == 0
    assert "System-One Decision Receipt" in _plain(result.stdout)
    assert "Auto-commit" in _plain(result.stdout)


@pytest.mark.asyncio
async def test_agent_halts_on_escalation(monkeypatch):
    """Verify the autonomous execution loop halts instead of committing an escalated delta."""
    import dry_exec.agent

    async def mock_exec(*args, **kwargs):
        return _escalated_delta()

    monkeypatch.setattr(
        dry_exec.agent.DryExecClient, "execute_ephemeral_action", mock_exec
    )

    agent = dry_exec.agent.Agent(task="Mutate ledger balance")

    with pytest.raises(DecisionEscalationError):
        await agent.run(auto_commit=True)


@pytest.mark.asyncio
async def test_agent_force_commits_escalated_delta(monkeypatch):
    """Verify a forced autonomous execution loop proceeds past escalation."""
    import dry_exec.agent

    async def mock_exec(*args, **kwargs):
        return _escalated_delta()

    monkeypatch.setattr(
        dry_exec.agent.DryExecClient, "execute_ephemeral_action", mock_exec
    )

    agent = dry_exec.agent.Agent(task="Mutate ledger balance")
    result = await agent.run(auto_commit=True, force=True)

    assert result.committed is True


def test_dry_run_decorator_halts_on_escalation(monkeypatch):
    """Verify the dry_run decorator halts on escalation unless the commit is forced."""
    import dry_exec
    import dry_exec.client

    async def mock_exec(*args, **kwargs):
        return _escalated_delta()

    monkeypatch.setattr(
        dry_exec.client.DryExecClient, "execute_ephemeral_action", mock_exec
    )

    @dry_exec.dry_run
    def credit_balance(amount: int) -> dict:
        return {"credited": amount}

    with pytest.raises(DecisionEscalationError):
        credit_balance(100)

    def settle_balance(amount: int) -> dict:
        return {"settled": amount}

    forced_result = dry_exec.run(settle_balance, 100, commit=True)
    assert forced_result.decision.noul_trigger is Noul.ESCALATE


def test_functional_runner_halts_on_escalation(monkeypatch):
    """Verify the functional runner halts on escalation for a direct command."""
    import dry_exec.client

    async def mock_exec(*args, **kwargs):
        return _escalated_delta()

    monkeypatch.setattr(
        dry_exec.client.DryExecClient, "execute_ephemeral_action", mock_exec
    )

    import dry_exec

    with pytest.raises(DecisionEscalationError):
        dry_exec.run("echo 'ephemeral testing'")


@REQUIRES_KERNEL_BOUNDARY
@pytest.mark.asyncio
async def test_system_one_violates_breached_calibrated_ceiling():
    """Verify the Rust decision layer routes a breached ceiling through the FFI boundary."""
    from dry_exec.client import DryExecClient

    client = DryExecClient()
    if client._ffi is None:
        pytest.skip("Rust FFI primitive is not loaded")

    request_body = '{"amount": 5000}'
    environment = Environment(
        name="escalation_calibration_env",
        allowed_mutation_targets={"payment"},
        allowed_api_endpoints={
            "POST /v1/charge": MockResponse(body='{"status": "captured"}')
        },
        memory_limit_bytes=1024 * 1024,
        max_mutated_bytes=8,
    )
    action = Action(
        action_id="act_calibrated_01",
        target_resource="payment",
        mutation_type="create_charge",
        payload={"amount": 5000},
    )

    delta = await client.execute_ephemeral_action(
        environment,
        action,
        request_to_trigger=("POST", "/v1/charge", request_body),
    )

    assert delta.decision is not None
    assert delta.decision.choice is Choice.VIOLATED
    assert delta.decision.noul_trigger is Noul.ESCALATE
    assert delta.decision.risk_score == 1.0
    assert "exceeds calibrated ceiling" in delta.decision.reason


@REQUIRES_KERNEL_BOUNDARY
@pytest.mark.asyncio
async def test_system_one_auto_commits_calibrated_delta_through_ffi():
    """Verify a calibrated state delta routes to auto-commit through the FFI boundary."""
    from dry_exec.client import DryExecClient

    client = DryExecClient()
    if client._ffi is None:
        pytest.skip("Rust FFI primitive is not loaded")

    environment = Environment(
        name="auto_commit_calibration_env",
        allowed_mutation_targets={"payment"},
        allowed_api_endpoints={
            "POST /v1/charge": MockResponse(body='{"status": "captured"}')
        },
        memory_limit_bytes=1024 * 1024,
    )
    action = Action(
        action_id="act_calibrated_02",
        target_resource="payment",
        mutation_type="create_charge",
        payload={"amount": 5000},
    )

    delta = await client.execute_ephemeral_action(
        environment,
        action,
        request_to_trigger=("POST", "/v1/charge", '{"amount": 5000}'),
    )

    assert len(delta.network_mutations) == 1
    assert delta.schema_breaches == 0
    assert delta.decision is not None
    assert delta.decision.choice is Choice.ALLOWED
    assert delta.decision.noul_trigger is Noul.AUTO_COMMIT


@REQUIRES_KERNEL_BOUNDARY
@pytest.mark.asyncio
async def test_system_one_blocks_refused_route_through_ffi():
    """Verify a route refused outside the schema routes to a categorical block."""
    from dry_exec.client import DryExecClient

    client = DryExecClient()
    if client._ffi is None:
        pytest.skip("Rust FFI primitive is not loaded")

    environment = Environment(
        name="refused_route_env",
        allowed_mutation_targets={"payment"},
        allowed_api_endpoints={
            "POST /v1/charge": MockResponse(body='{"status": "captured"}')
        },
        memory_limit_bytes=1024 * 1024,
    )
    action = Action(
        action_id="act_calibrated_03",
        target_resource="payment",
        mutation_type="create_charge",
        payload={"amount": 5000},
    )

    delta = await client.execute_ephemeral_action(
        environment,
        action,
        request_to_trigger=("POST", "/v1/unregistered", "{}"),
    )

    assert delta.schema_breaches == 1
    assert delta.decision is not None
    assert delta.decision.choice is Choice.BLOCKED
    assert delta.decision.noul_trigger is Noul.ESCALATE
    assert "refused 1 route(s)" in delta.decision.reason
