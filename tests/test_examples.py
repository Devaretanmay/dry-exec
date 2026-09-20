"""Verification suite for Loop 7: Production-Ready Example Workflows."""

import pytest
import dry_exec.client

try:
    from examples.use_cases.api_payment_exploration import run_api_payment_exploration
    from examples.use_cases.type_safe_db_migration import run_self_correcting_db_migration
except ImportError:
    from examples.api_payment_exploration import run_api_payment_exploration
    from examples.type_safe_db_migration import run_self_correcting_db_migration


@pytest.fixture(autouse=True)
def fallback_ffi_if_host_non_linux(monkeypatch):
    """Fallback mock when tests run on non-Linux hosts where Rust kernel primitives are disabled."""
    if dry_exec.client._dry_exec_ffi is None:
        from dry_exec.models import ByteDelta, InterceptedRequest, PageMutation, StateDelta

        async def mock_execute(self, env, action, trigger_blocked_syscall=False, request_to_trigger=None):
            env.validate_action(action)
            net_mutations = []
            if request_to_trigger:
                method, url, body = request_to_trigger
                net_mutations.append(
                    InterceptedRequest(
                        method=method,
                        url=url,
                        headers={},
                        request_body=body.encode(),
                        response_status=200,
                        response_body=b'{"status": "captured", "charge_id": "ch_mock_9988", "amount": 2500}',
                    )
                )
            return StateDelta(
                memory_mutations=[
                    PageMutation(
                        page_index=0,
                        page_address=0x1000,
                        deltas=[ByteDelta(offset=0, original=b"\x00", mutated=b"\xde\xad\xbe\xef")],
                    )
                ],
                fs_mutations=[],
                network_mutations=net_mutations,
                total_bytes_mutated=4,
                duration_nanos=450_000,
            )

        monkeypatch.setattr(dry_exec.client.DryExecClient, "execute_ephemeral_action", mock_execute)


@pytest.mark.asyncio
async def test_example_type_safe_db_migration():
    """Verify example workflow 1 executes and self-corrects without error."""
    await run_self_correcting_db_migration()


@pytest.mark.asyncio
async def test_example_api_payment_exploration():
    """Verify example workflow 2 executes deterministic network proxy interception."""
    await run_api_payment_exploration()


@pytest.mark.asyncio
async def test_example_quickstart():
    """Verify getting started quickstart example executes without error."""
    from examples.getting_started.quickstart import main as quickstart_main
    await quickstart_main()


def test_example_langchain_tool():
    """Verify LangChain tool wrapper executes within ephemeral boundary."""
    from examples.integrations.langchain_tool import DryExecLangChainTool
    tool = DryExecLangChainTool()
    receipt = tool._run(
        action_id="act_lc_test",
        target_resource="database",
        mutation_type="schema_migration",
        payload={"query": "ALTER TABLE test;"},
    )
    assert "Dry-run executed successfully" in receipt
