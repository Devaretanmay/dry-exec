"""End-to-end proof that a workspace trial captures mutations and applies them exactly once."""

import sys
from pathlib import Path

import pytest

from dry_exec import Action, DryExecClient, Environment
from dry_exec.client import _dry_exec_ffi

pytestmark = pytest.mark.skipif(
    sys.platform not in ("linux", "darwin") or _dry_exec_ffi is None,
    reason="Requires the native execution boundary and Rust FFI primitive",
)


def _environment() -> Environment:
    return Environment(name="workspace_trial_real", allowed_mutation_targets={"*"})


@pytest.mark.asyncio
async def test_workspace_trial_commit_applies_captured_mutations(tmp_path: Path):
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    (workspace / "state.txt").write_text("before")
    (workspace / "keep.txt").write_text("keep")

    action = Action(
        action_id="workspace_trial_commit",
        target_resource="shell_command",
        mutation_type="execute",
        payload={
            "command": [
                "/bin/sh",
                "-c",
                "printf after > state.txt; printf created > created.txt; rm keep.txt",
            ]
        },
    )

    client = DryExecClient()
    trial = await client.run_trial(_environment(), action, workspace_dir=str(workspace))

    assert (workspace / "state.txt").read_text() == "before"
    assert (workspace / "keep.txt").exists()
    assert not (workspace / "created.txt").exists()

    mutation_types = {mutation.mutation_type for mutation in trial.delta.fs_mutations}
    assert {"modified", "created", "deleted"}.issubset(mutation_types)

    await trial.commit()

    assert (workspace / "state.txt").read_text() == "after"
    assert (workspace / "created.txt").read_text() == "created"
    assert not (workspace / "keep.txt").exists()


@pytest.mark.asyncio
async def test_workspace_trial_rollback_leaves_target_untouched(tmp_path: Path):
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    (workspace / "state.txt").write_text("before")

    action = Action(
        action_id="workspace_trial_rollback",
        target_resource="shell_command",
        mutation_type="execute",
        payload={"command": ["/bin/sh", "-c", "printf changed > state.txt"]},
    )

    client = DryExecClient()
    trial = await client.run_trial(_environment(), action, workspace_dir=str(workspace))

    await trial.rollback()

    assert trial.is_rolled_back
    assert (workspace / "state.txt").read_text() == "before"
