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
    """Verify @dry_exec.dry_run decorator wraps synchronous function execution."""
    import dry_exec

    @dry_exec.dry_run
    def update_balance(amount: int):
        return {"new_balance": amount}

    delta = update_balance(100)
    assert isinstance(delta, dry_exec.StateDelta)
    assert delta.total_bytes_mutated >= 0
    assert delta.duration_nanos > 0


@pytest.mark.asyncio
async def test_dex_dry_run_async_decorator():
    """Verify @dry_exec.dry_run decorator wraps asynchronous coroutine execution."""
    import dry_exec

    @dry_exec.dry_run
    async def async_mutation(key: str, val: str):
        return {key: val}

    delta = await async_mutation("status", "active")
    assert isinstance(delta, dry_exec.StateDelta)
    assert delta.total_bytes_mutated >= 0
    assert delta.duration_nanos > 0


def test_dex_run_functional_callable():
    """Verify dry_exec.run functional helper on callable."""
    import dry_exec

    def test_op(x, y):
        return x + y

    delta = dry_exec.run(test_op, 10, 20)
    assert isinstance(delta, dry_exec.StateDelta)
    assert delta.total_bytes_mutated >= 0
    assert delta.duration_nanos > 0


def test_dex_run_command_string():
    """Verify dry_exec.run functional helper on direct shell command."""
    import dry_exec

    delta = dry_exec.run("echo 'ephemeral testing'")
    assert isinstance(delta, dry_exec.StateDelta)
    assert delta.total_bytes_mutated >= 0
    assert delta.duration_nanos > 0


@pytest.mark.asyncio
async def test_dex_agent_three_line():
    """Verify 3-line Agent initialization and autonomous loop execution."""
    from dry_exec import Agent

    agent = Agent(task="Migrate customer records to v2 schema")
    result = await agent.run()

    assert result.success is True
    assert result.trials_conducted >= 1
    assert result.committed is True
    assert result.final_delta is not None


def test_de_cli_direct_command():
    """Verify 'de' CLI direct command execution without YAML configuration."""
    typer = pytest.importorskip("typer")
    from typer.testing import CliRunner
    from dry_exec.cli import app

    runner = CliRunner()
    result = runner.invoke(app, ["echo 'hello world'", "--commit"])
    assert result.exit_code == 0
    assert "State Delta Receipt" in result.stdout or "Ephemeral State Exploration" in result.stdout


def test_de_cli_version():
    """Verify 'de' version output."""
    typer = pytest.importorskip("typer")
    from typer.testing import CliRunner
    from dry_exec.cli import app

    runner = CliRunner()
    result = runner.invoke(app, ["version"])
    assert result.exit_code == 0
    assert "de" in result.stdout
    assert "dry-exec" in result.stdout


def test_dex_compat_shim():
    """Verify dex module functions as backward compatibility shim for dry_exec."""
    import dex
    assert hasattr(dex, "Agent")
    assert hasattr(dex, "dry_run")
    assert hasattr(dex, "run")
    assert hasattr(dex, "StateDelta")

