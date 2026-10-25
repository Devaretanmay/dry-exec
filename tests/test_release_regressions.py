"""Regression checks for release-critical commit and adapter boundaries."""

import asyncio
import threading
from pathlib import Path

import pytest

from dry_exec import Action, DryExecClient, Environment, FsMutation, StateDelta, Trial
from dry_exec.mcp import sandboxed_mcp_call


def test_async_callable_runner_awaits_function(tmp_path):
    import subprocess
    import sys
    from dry_exec import _callable_command

    async def write_receipt(path):
        with open(path, "w") as handle:
            handle.write("awaited")

    receipt = tmp_path / "receipt"
    command, script = _callable_command(write_receipt, (str(receipt),), {})
    try:
        assert command[0] == sys.executable
        result = subprocess.run(
            [sys.executable, *command[1:]], capture_output=True, timeout=10
        )
        assert result.returncode == 0, result.stderr
        assert receipt.read_text() == "awaited"
    finally:
        Path(script).unlink(missing_ok=True)


def test_mcp_runner_handles_multiline_decorators(tmp_path):
    import json
    import subprocess
    import sys
    from dry_exec.mcp import _prepare_tool_script

    def configure(**kwargs):
        return lambda fn: fn

    @configure(
        enabled=True,
    )
    def tool():
        return {"executed": True}

    script = tmp_path / "runner.py"
    script.write_text(_prepare_tool_script(tool, (), {}, "result.json"))
    result = subprocess.run(
        [sys.executable, str(script)], cwd=tmp_path, capture_output=True, timeout=10
    )
    assert result.returncode == 0, result.stderr
    assert json.loads((tmp_path / "result.json").read_text()) == {"executed": True}


@pytest.mark.asyncio
async def test_commit_cancellation_waits_for_workspace_writer(tmp_path):
    workspace = tmp_path / "workspace"
    scratch = tmp_path / "scratch"
    workspace.mkdir()
    scratch.mkdir()
    (scratch / "value").write_text("new")
    trial = Trial(
        StateDelta(
            total_bytes_mutated=3,
            duration_nanos=0,
            fs_mutations=[FsMutation(mutation_type="created", path="value")],
        ),
        Environment(),
        Action(action_id="cancel", target_resource="file", mutation_type="write"),
        scratch_dir=str(scratch),
        workspace_dir=str(workspace),
    )
    started = threading.Event()
    release = threading.Event()
    original = trial._copy_file_durable

    def delayed_copy(source, target):
        started.set()
        assert release.wait(5)
        original(source, target)

    trial._copy_file_durable = delayed_copy
    task = asyncio.create_task(trial.commit())
    try:
        assert await asyncio.to_thread(started.wait, 5)
        task.cancel()
        await asyncio.sleep(0)
        assert not task.done(), "commit returned while its writer was active"
    finally:
        release.set()
        with pytest.raises(asyncio.CancelledError):
            await task
    assert trial.is_committed
    assert (workspace / "value").read_text() == "new"
    assert not scratch.exists()


@pytest.mark.asyncio
async def test_mcp_rejects_unserializable_callable_without_noop_execution(tmp_path):
    class NeverExecuteClient:
        async def run_trial(self, **kwargs):
            pytest.fail("an unserializable tool must fail before execution")

    with pytest.raises((TypeError, ValueError)):
        await sandboxed_mcp_call(len, [1], client=NeverExecuteClient())


@pytest.mark.asyncio
async def test_trial_keeps_absolute_workspace_target_after_cwd_change(
    tmp_path, monkeypatch
):
    from tests.test_trial_and_mcp import MockFFI

    (tmp_path / "workspace").mkdir()
    monkeypatch.chdir(tmp_path)
    trial = await DryExecClient(ffi_module=MockFFI()).run_trial(
        Environment(),
        Action(action_id="relative", target_resource="file", mutation_type="write"),
        workspace_dir="workspace",
    )
    try:
        assert Path(trial._workspace_dir).is_absolute()
    finally:
        await trial.rollback()


@pytest.mark.asyncio
async def test_mcp_cleans_runner_when_boundary_fails(tmp_path, monkeypatch):
    import dry_exec.mcp as mcp

    monkeypatch.setattr(mcp.tempfile, "tempdir", str(tmp_path))

    class FailingClient:
        async def run_trial(self, **kwargs):
            raise RuntimeError("boundary setup failed")

    def tool():
        return 1

    with pytest.raises(RuntimeError, match="boundary setup failed"):
        await sandboxed_mcp_call(tool, client=FailingClient())
    assert not list(tmp_path.glob("dry_exec_mcp_*.py"))


@pytest.mark.asyncio
async def test_mcp_rolls_back_when_decision_escalates(tmp_path):
    from tests.test_trial_and_mcp import MockFFI
    from dry_exec import DecisionEscalationError

    ffi = MockFFI()
    ffi.delta_dict["decision"].update(noul_trigger="escalate", choice="blocked")

    class TrackingClient(DryExecClient):
        async def run_trial(self, **kwargs):
            self.last_trial = await super().run_trial(**kwargs)
            return self.last_trial

    client = TrackingClient(ffi_module=ffi)

    def tool():
        return 1

    with pytest.raises(DecisionEscalationError):
        await sandboxed_mcp_call(
            tool, client=client, workspace_dir=str(tmp_path), auto_commit=True
        )
    assert client.last_trial.is_rolled_back
    assert not Path(client.last_trial._scratch_dir).exists()


@pytest.mark.asyncio
@pytest.mark.parametrize("kind", ["symlink", "fifo"])
async def test_mcp_rejects_non_regular_result_files(tmp_path, kind):
    import os
    import re
    from tests.test_trial_and_mcp import MockFFI

    secret = tmp_path / "private.txt"
    secret.write_text("must not be read")

    class MaliciousResultClient(DryExecClient):
        async def run_trial(self, **kwargs):
            self.last_trial = await super().run_trial(**kwargs)
            script = Path(kwargs["action"].payload["command"][1]).read_text()
            output_name = re.search(r"\.dex_mcp_out_[0-9a-f]+\.json", script).group()
            output = Path(self.last_trial._scratch_dir) / output_name
            if kind == "symlink":
                output.symlink_to(secret)
            else:
                os.mkfifo(output)
            return self.last_trial

    client = MaliciousResultClient(ffi_module=MockFFI())

    def tool():
        return 1

    with pytest.raises((OSError, ValueError)):
        await sandboxed_mcp_call(tool, client=client)
    assert client.last_trial.is_rolled_back
    assert not Path(client.last_trial._scratch_dir).exists()
    assert secret.read_text() == "must not be read"


@pytest.mark.asyncio
async def test_failed_commit_restoration_keeps_recovery_backup(tmp_path, monkeypatch):
    import os

    workspace = tmp_path / "workspace"
    scratch = tmp_path / "scratch"
    workspace.mkdir()
    scratch.mkdir()
    (workspace / "state").write_text("original")
    (scratch / "state").write_text("changed")
    trial = Trial(
        StateDelta(
            total_bytes_mutated=7,
            duration_nanos=0,
            fs_mutations=[FsMutation(mutation_type="modified", path="state")],
        ),
        Environment(),
        Action(action_id="restore", target_resource="file", mutation_type="write"),
        scratch_dir=str(scratch),
        workspace_dir=str(workspace),
    )
    replace = os.replace

    def fail_restore(source, target):
        if Path(source).parent.name.startswith(".dex_journal_"):
            raise OSError("restore denied")
        replace(source, target)

    def fail_copy(source, target):
        raise OSError("copy failed")

    monkeypatch.setattr(os, "replace", fail_restore)
    monkeypatch.setattr(trial, "_copy_file_durable", fail_copy)
    with pytest.raises((OSError, RuntimeError)):
        await trial.commit()
    backups = list(workspace.glob(".dex_journal_*/*"))
    assert len(backups) == 1, "failed recovery must retain the original data"
    assert backups[0].read_text() == "original"
    assert not trial.is_committed
    await trial.rollback()
    assert backups[0].exists()
