"""Loop 15: real user-command execution inside the native Linux boundary."""

import sys
from pathlib import Path

import pytest

from dry_exec import Action, DryExecClient, Environment, MockResponse, StateDelta

pytestmark = pytest.mark.skipif(
    sys.platform != "linux", reason="Requires Linux namespaces and native FFI"
)


@pytest.mark.asyncio
async def test_real_command_captures_output_and_filesystem_delta(tmp_path: Path):
    host_path = Path("/tmp/dex_loop15_host_must_not_exist.txt")
    host_path.unlink(missing_ok=True)
    sandbox_path = "/tmp/dry_exec_ephemeral/dex_loop15_created.txt"
    env = Environment(
        name="loop15_real_command",
        allowed_mutation_targets={"*"},
        allowed_filesystem_roots=["/tmp/dry_exec_ephemeral"],
    )
    action = Action(
        action_id="loop15_command_001",
        target_resource="shell_command",
        mutation_type="execute",
        payload={
            "command": [
                "/bin/sh",
                "-c",
                f"printf 'hello world\\n'; printf 'sandbox' > {sandbox_path}",
            ]
        },
    )

    delta: StateDelta = await DryExecClient().execute_ephemeral_action(env, action)

    assert delta.stdout == b"hello world\n"
    assert delta.stderr == b""
    assert delta.exit_code == 0
    assert any(
        mutation.path.endswith("dex_loop15_created.txt")
        for mutation in delta.fs_mutations
    )
    assert not host_path.exists()


@pytest.mark.asyncio
async def test_real_command_nonzero_exit_is_reported():
    env = Environment(name="loop15_exit", allowed_mutation_targets={"*"})
    action = Action(
        action_id="loop15_command_002",
        target_resource="shell_command",
        mutation_type="execute",
        payload={"command": ["/bin/sh", "-c", "printf failure >&2; exit 7"]},
    )

    delta = await DryExecClient().execute_ephemeral_action(env, action)

    assert delta.stderr == b"failure"
    assert delta.exit_code == 7


@pytest.mark.asyncio
async def test_real_command_uses_transparent_proxy_environment():
    env = Environment(
        name="loop16_proxy",
        allowed_mutation_targets={"*"},
        allowed_api_endpoints={
            "GET http://httpbin.org/get": MockResponse(
                status_code=200, body='{"simulated": true}'
            )
        },
    )
    action = Action(
        action_id="loop16_proxy_001",
        target_resource="shell_command",
        mutation_type="execute",
        payload={
            "command": [
                "python3",
                "-c",
                "from urllib.request import urlopen; print(urlopen('http://httpbin.org/get').read().decode())",
            ]
        },
    )

    delta = await DryExecClient().execute_ephemeral_action(env, action)

    assert delta.exit_code == 0
    assert b'"simulated": true' in delta.stdout
    assert len(delta.network_mutations) == 1
    assert delta.network_mutations[0].url == "http://httpbin.org/get"


@pytest.mark.asyncio
async def test_real_command_https_proxy_returns_mock_payload():
    env = Environment(
        name="loop20_https_proxy",
        allowed_mutation_targets={"*"},
        allowed_api_endpoints={
            "GET https://httpbin.org/get": MockResponse(
                status_code=200, body='{"simulated": true}'
            )
        },
    )
    action = Action(
        action_id="loop20_https_proxy_001",
        target_resource="shell_command",
        mutation_type="execute",
        payload={
            "command": [
                "python3",
                "-c",
                (
                    "import ssl; from urllib.request import urlopen; "
                    "print(urlopen('https://httpbin.org/get', "
                    "context=ssl._create_unverified_context()).read().decode())"
                ),
            ]
        },
    )

    delta = await DryExecClient().execute_ephemeral_action(env, action)

    assert delta.exit_code == 0
    assert b'"simulated": true' in delta.stdout
    assert len(delta.network_mutations) == 1
    assert delta.network_mutations[0].url == "https://httpbin.org/get"
