"""Verification suite for Loop 5 and Loop 6: Developer CLI & Observability."""

import json
from pathlib import Path
from typer.testing import CliRunner
from dry_exec.cli import app

runner = CliRunner()


def test_cli_version():
    """Verify CLI version command outputs kernel primitive information."""
    result = runner.invoke(app, ["version"])
    assert result.exit_code == 0
    assert "dry-exec" in result.stdout
    assert "Ephemeral Kernel Isolation" in result.stdout


def test_cli_inspect(tmp_path: Path):
    """Verify CLI inspect command parses and summarizes environment boundaries."""
    env_file = tmp_path / "test_env.json"
    env_file.write_text(
        json.dumps({
            "name": "cli_test_env",
            "allowed_mutation_targets": ["user_balance", "order_status"],
            "memory_limit_bytes": 2048,
        }),
        encoding="utf-8",
    )

    result = runner.invoke(app, ["inspect", "--config", str(env_file)])
    assert result.exit_code == 0
    assert "cli_test_env" in result.stdout
    assert "user_balance" in result.stdout


def test_cli_ephemeral_run(tmp_path: Path, monkeypatch):
    """Verify CLI run command executes action and displays Rich delta visualization."""
    env_file = tmp_path / "env.json"
    env_file.write_text(
        json.dumps({
            "name": "ledger_env",
            "allowed_mutation_targets": ["balance"],
            "memory_limit_bytes": 1024 * 1024,
        }),
        encoding="utf-8",
    )

    action_file = tmp_path / "action.json"
    action_file.write_text(
        json.dumps({
            "action_id": "cli_act_01",
            "target_resource": "balance",
            "mutation_type": "credit",
            "payload": {"amount": 100},
        }),
        encoding="utf-8",
    )

    import dry_exec.cli
    if dry_exec.cli.DryExecClient()._ffi is None:
        from dry_exec.models import ByteDelta, PageMutation, StateDelta

        async def mock_exec(*args, **kwargs):
            return StateDelta(
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
                duration_nanos=500_000,
            )

        monkeypatch.setattr(dry_exec.cli.DryExecClient, "execute_ephemeral_action", mock_exec)

    result = runner.invoke(
        app,
        ["run", "--config", str(env_file), "--action", str(action_file), "--commit"],
    )
    assert result.exit_code == 0
    assert "Ephemeral State Exploration" in result.stdout
    assert "State Delta Receipt" in result.stdout


def test_cli_schema_violation_rejection(tmp_path: Path):
    """Verify CLI run command rejects unauthorized actions synchronously with code 2."""
    env_file = tmp_path / "env.json"
    env_file.write_text(
        json.dumps({
            "name": "ledger_env",
            "allowed_mutation_targets": ["balance"],
        }),
        encoding="utf-8",
    )

    invalid_action_file = tmp_path / "invalid_action.json"
    invalid_action_file.write_text(
        json.dumps({
            "action_id": "cli_act_invalid",
            "target_resource": "unauthorized_column",
            "mutation_type": "drop",
            "payload": {},
        }),
        encoding="utf-8",
    )

    result = runner.invoke(
        app,
        ["run", "--config", str(env_file), "--action", str(invalid_action_file)],
    )
    assert result.exit_code == 2
    assert "Schema Boundary Violation" in result.stdout
