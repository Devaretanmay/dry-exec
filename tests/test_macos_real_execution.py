"""macOS native real-command verification."""

import sys

import pytest

from dry_exec import Action, DryExecClient, Environment

pytestmark = pytest.mark.skipif(
    sys.platform != "darwin", reason="Requires macOS Seatbelt and APFS backend"
)


@pytest.mark.asyncio
async def test_macos_real_command_captures_stdout():
    action = Action(
        action_id="macos_echo_001",
        target_resource="shell_command",
        mutation_type="execute",
        payload={"command": ["/bin/echo", "hello"]},
    )
    delta = await DryExecClient().execute_ephemeral_action(
        Environment(name="macos-real-command", allowed_mutation_targets={"*"}),
        action,
    )

    assert delta.exit_code == 0
    assert delta.stdout == b"hello\n"
    assert delta.stderr == b""


@pytest.mark.asyncio
async def test_macos_real_command_separates_stdout_and_stderr():
    action = Action(
        action_id="macos_stdio_002",
        target_resource="shell_command",
        mutation_type="execute",
        payload={
            "command": [
                "/bin/sh",
                "-c",
                "printf 'std_out_data\n'; printf 'std_err_data\n' >&2",
            ]
        },
    )
    delta = await DryExecClient().execute_ephemeral_action(
        Environment(name="macos-stdio-separation", allowed_mutation_targets={"*"}),
        action,
    )

    assert delta.exit_code == 0
    assert delta.stdout == b"std_out_data\n"
    assert delta.stderr == b"std_err_data\n"


@pytest.mark.asyncio
async def test_macos_real_command_executes_system_binary_in_allowed_exec_path():
    action = Action(
        action_id="macos_date_003",
        target_resource="shell_command",
        mutation_type="execute",
        payload={"command": ["/bin/date", "+%Y"]},
    )
    delta = await DryExecClient().execute_ephemeral_action(
        Environment(name="macos-date-exec", allowed_mutation_targets={"*"}),
        action,
    )

    assert delta.exit_code == 0
    assert delta.stdout.strip().isdigit()
    assert len(delta.stdout.strip()) == 4
    assert delta.stderr == b""


@pytest.mark.asyncio
async def test_macos_real_command_reports_filesystem_delta(tmp_path):
    (tmp_path / "existing.txt").write_text("before")

    action = Action(
        action_id="macos_fs_delta_005",
        target_resource="shell_command",
        mutation_type="execute",
        payload={
            "command": [
                "/bin/sh",
                "-c",
                "printf 'after' > existing.txt; printf 'new' > created.txt",
            ]
        },
    )
    delta = await DryExecClient().execute_ephemeral_action(
        Environment(
            name="macos-fs-delta",
            allowed_mutation_targets={"*"},
            allowed_filesystem_roots=[str(tmp_path)],
        ),
        action,
    )

    assert delta.exit_code == 0
    mutations = {mutation.path: mutation for mutation in delta.fs_mutations}
    assert mutations["created.txt"].mutation_type == "created"
    modified = mutations["existing.txt"]
    assert modified.mutation_type == "modified"
    assert b"after" in b"".join(part.mutated for part in modified.deltas)


@pytest.mark.asyncio
async def test_macos_credential_paths_are_denied():
    import os

    home = os.environ.get("HOME", "/tmp")
    action = Action(
        action_id="macos_secret_004",
        target_resource="shell_command",
        mutation_type="execute",
        payload={"command": ["/bin/sh", "-c", f"ls {home}/.ssh 2>&1"]},
    )
    delta = await DryExecClient().execute_ephemeral_action(
        Environment(name="macos-cred-deny", allowed_mutation_targets={"*"}),
        action,
    )

    # If ~/.ssh exists, reading is denied by Seatbelt (Operation not permitted)
    if os.path.exists(f"{home}/.ssh"):
        assert delta.exit_code != 0 or b"Operation not permitted" in delta.stdout
        assert (
            b"Operation not permitted" in delta.stdout
            or b"denied" in delta.stdout
            or delta.exit_code != 0
        )
