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
