"""Verification suite for Loop 11: The 'Dex' Ergonomics Overhaul."""

import pytest
import dry_exec.client
import dex


@pytest.fixture(autouse=True)
def fallback_ffi_if_host_non_linux(monkeypatch):
    """Fallback mock when tests run on non-Linux hosts where Rust kernel primitives are disabled."""
    if dry_exec.client._dry_exec_ffi is None:
        from dry_exec.models import ByteDelta, PageMutation, StateDelta

        async def mock_execute(self, env, action, trigger_blocked_syscall=False, request_to_trigger=None):
            env.validate_action(action)
            return StateDelta(
                memory_mutations=[
                    PageMutation(
                        page_index=0,
                        page_address=0x1000,
                        deltas=[ByteDelta(offset=0, original=b"\x00", mutated=b"\xaa\xbb")],
                    )
                ],
                fs_mutations=[],
                network_mutations=[],
                total_bytes_mutated=2,
                duration_nanos=320_000,
            )

        monkeypatch.setattr(dry_exec.client.DryExecClient, "execute_ephemeral_action", mock_execute)


def test_dex_dry_run_sync_decorator():
    """Verify @dex.dry_run decorator wraps synchronous function execution."""
    @dex.dry_run
    def update_balance(amount: int):
        return {"new_balance": amount}

    delta = update_balance(100)
    assert isinstance(delta, dex.StateDelta)
    assert delta.total_bytes_mutated >= 0
    assert delta.duration_nanos > 0


@pytest.mark.asyncio
async def test_dex_dry_run_async_decorator():
    """Verify @dex.dry_run decorator wraps asynchronous coroutine execution."""
    @dex.dry_run
    async def async_mutation(key: str, val: str):
        return {key: val}

    delta = await async_mutation("status", "active")
    assert isinstance(delta, dex.StateDelta)
    assert delta.total_bytes_mutated >= 0
    assert delta.duration_nanos > 0


def test_dex_run_functional_callable():
    """Verify dex.run functional helper on callable."""
    def test_op(x, y):
        return x + y

    delta = dex.run(test_op, 10, 20)
    assert isinstance(delta, dex.StateDelta)
    assert delta.total_bytes_mutated >= 0
    assert delta.duration_nanos > 0


def test_dex_run_command_string():
    """Verify dex.run functional helper on direct shell command."""
    delta = dex.run("echo 'ephemeral testing'")
    assert isinstance(delta, dex.StateDelta)
    assert delta.total_bytes_mutated >= 0
    assert delta.duration_nanos > 0


@pytest.mark.asyncio
async def test_dex_agent_three_line():
    """Verify 3-line Agent initialization and autonomous loop execution."""
    agent = dex.Agent(task="Migrate customer records to v2 schema")
    result = await agent.run()

    assert result.success is True
    assert result.trials_conducted >= 1
    assert result.committed is True
    assert result.final_delta is not None


def test_dex_cli_direct_command():
    """Verify dex CLI direct command execution without YAML configuration."""
    typer = pytest.importorskip("typer")
    from typer.testing import CliRunner
    from dex.cli import app

    runner = CliRunner()
    result = runner.invoke(app, ["echo 'hello world'", "--commit"])
    assert result.exit_code == 0
    assert "State Delta Receipt" in result.stdout or "Ephemeral State Exploration" in result.stdout


def test_dex_cli_version():
    """Verify dex version output."""
    typer = pytest.importorskip("typer")
    from typer.testing import CliRunner
    from dex.cli import app

    runner = CliRunner()
    result = runner.invoke(app, ["version"])
    assert result.exit_code == 0
    assert "dex" in result.stdout
    assert "dry-exec" in result.stdout
