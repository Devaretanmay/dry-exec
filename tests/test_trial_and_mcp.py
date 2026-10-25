"""Verification suite for Trial primitive, MCP adapter, deferred execution hazards, and credential masking."""

from pathlib import Path
import tempfile
import pytest

from dry_exec import (
    Action,
    Choice,
    DecisionEscalationError,
    DecisionReceipt,
    DryExecClient,
    Environment,
    FsMutation,
    Noul,
    SandboxedToolResult,
    StateDelta,
    Trial,
    sandboxed_mcp_call,
    sandboxed_mcp_tool,
)


def _mock_delta(
    mutations=None,
    choice=Choice.ALLOWED,
    noul_trigger=Noul.AUTO_COMMIT,
    risk_score=0.1,
    hazards=0,
) -> StateDelta:
    return StateDelta(
        fs_mutations=mutations or [],
        deferred_execution_hazards=hazards,
        total_bytes_mutated=100,
        duration_nanos=10_000,
        decision=DecisionReceipt(
            choice=choice,
            risk_score=risk_score,
            noul_trigger=noul_trigger,
            reason="Mock decision receipt",
        ),
    )


class MockFFI:
    def __init__(self, delta_dict=None):
        self.delta_dict = delta_dict or {
            "total_bytes_mutated": 42,
            "schema_breaches": 0,
            "duration_nanos": 12000,
            "stdout": b"mock stdout",
            "stderr": b"",
            "exit_code": 0,
            "memory_mutations": [],
            "fs_mutations": [],
            "network_mutations": [],
            "decision": {
                "choice": "allowed",
                "risk_score": 0.05,
                "noul_trigger": "auto_commit",
                "reason": "OK",
            },
        }

    def execute_isolated_action(self, *args, **kwargs):
        return dict(self.delta_dict)


@pytest.mark.asyncio
async def test_trial_commit_and_rollback_workspace_sync():
    """Verify that Trial copies mutations on commit and discards them on rollback."""
    with tempfile.TemporaryDirectory() as target_ws:
        ws_file = Path(target_ws) / "target.txt"
        ws_file.write_text("initial content")

        with tempfile.TemporaryDirectory() as scratch_dir:
            scratch_file = Path(scratch_dir) / "target.txt"
            scratch_file.write_text("mutated content")

            mutation = FsMutation(
                mutation_type="modified",
                path=str(scratch_file),
            )
            delta = _mock_delta(mutations=[mutation])
            env = Environment()
            action = Action(
                action_id="trial_1",
                target_resource="target.txt",
                mutation_type="modify",
            )

            # 1. Test Rollback: target remains unchanged
            trial = Trial(
                delta=delta,
                environment=env,
                action=action,
                scratch_dir=scratch_dir,
                workspace_dir=target_ws,
            )
            await trial.rollback()
            assert trial.is_rolled_back
            assert ws_file.read_text() == "initial content"

        # 2. Test Commit: target is updated atomically
        with tempfile.TemporaryDirectory() as scratch_dir_2:
            scratch_file_2 = Path(scratch_dir_2) / "target.txt"
            scratch_file_2.write_text("committed content")

            mutation_2 = FsMutation(
                mutation_type="modified",
                path=str(scratch_file_2),
            )
            delta_2 = _mock_delta(mutations=[mutation_2])
            trial_2 = Trial(
                delta=delta_2,
                environment=env,
                action=action,
                scratch_dir=scratch_dir_2,
                workspace_dir=target_ws,
            )
            await trial_2.commit()
            assert trial_2.is_committed
            assert ws_file.read_text() == "committed content"


@pytest.mark.asyncio
async def test_trial_context_manager_auto_rollback():
    """Verify trial context manager automatically rolls back uncommitted changes upon exit."""
    with tempfile.TemporaryDirectory() as target_ws:
        ws_file = Path(target_ws) / "config.json"
        ws_file.write_text('{"v": 1}')

        with tempfile.TemporaryDirectory() as scratch_dir:
            scratch_file = Path(scratch_dir) / "config.json"
            scratch_file.write_text('{"v": 2}')

            mutation = FsMutation(mutation_type="modified", path=str(scratch_file))
            delta = _mock_delta(mutations=[mutation])
            env = Environment()
            action = Action(
                action_id="trial_ctx",
                target_resource="config.json",
                mutation_type="modify",
            )

            async with Trial(
                delta=delta,
                environment=env,
                action=action,
                scratch_dir=scratch_dir,
                workspace_dir=target_ws,
            ) as trial:
                # Deliberately exit without commit
                pass

            assert trial.is_rolled_back
            assert ws_file.read_text() == '{"v": 1}'


@pytest.mark.asyncio
async def test_trial_commit_halts_on_escalation_unless_forced():
    """Verify that committing an escalated trial halts with DecisionEscalationError."""
    delta = _mock_delta(
        choice=Choice.VIOLATED,
        noul_trigger=Noul.ESCALATE,
        risk_score=0.9,
    )
    env = Environment()
    action = Action(
        action_id="act_viol",
        target_resource="db",
        mutation_type="drop",
    )
    trial = Trial(delta=delta, environment=env, action=action)

    with pytest.raises(DecisionEscalationError):
        await trial.commit(force=False)

    assert not trial.is_committed

    # When force=True, escalation gate is bypassed
    await trial.commit(force=True)
    assert trial.is_committed


@pytest.mark.asyncio
async def test_deferred_execution_hazard_detection():
    """Verify that mutations targeting git hooks or shell configs are marked as hazards."""
    hook_mut = FsMutation(
        mutation_type="created",
        path=".git/hooks/pre-push",
        is_deferred_execution_hazard=True,
    )
    normal_mut = FsMutation(
        mutation_type="created",
        path="src/app.py",
        is_deferred_execution_hazard=False,
    )
    delta = _mock_delta(mutations=[hook_mut, normal_mut], hazards=1)

    assert delta.deferred_execution_hazards == 1
    assert delta.fs_mutations[0].is_deferred_execution_hazard
    assert not delta.fs_mutations[1].is_deferred_execution_hazard


@pytest.mark.asyncio
async def test_credential_phantom_token_masking():
    """Verify that real secrets are scrubbed and replaced with phantom tokens."""
    raw_delta = {
        "stdout": b"Logged into upstream with token: sk-live-secret-12345",
        "stderr": b"Error authenticating with sk-live-secret-12345",
        "total_bytes_mutated": 50,
        "schema_breaches": 0,
        "duration_nanos": 5000,
        "exit_code": 0,
        "fs_mutations": [],
        "network_mutations": [
            {
                "method": "POST",
                "url": "https://api.example.com?key=sk-live-secret-12345",
                "headers": {"Authorization": "Bearer sk-live-secret-12345"},
                "request_body": b'{"token": "sk-live-secret-12345"}',
                "response_status": 200,
                "response_body": b'{"echo": "sk-live-secret-12345"}',
            }
        ],
        "decision": {
            "choice": "allowed",
            "risk_score": 0.1,
            "noul_trigger": "auto_commit",
            "reason": "Request allowed for sk-live-secret-12345",
        },
    }

    env = Environment(credential_masks={"PHANTOM_KEY": "sk-live-secret-12345"})
    action = Action(
        action_id="act_sec",
        target_resource="api",
        mutation_type="query",
    )

    client = DryExecClient(ffi_module=MockFFI(raw_delta))
    delta = await client.execute_ephemeral_action(env, action)

    # Asserts that real secret never appears in any field
    assert b"sk-live-secret-12345" not in delta.stdout
    assert b"PHANTOM_KEY" in delta.stdout

    assert b"sk-live-secret-12345" not in delta.stderr
    assert b"PHANTOM_KEY" in delta.stderr

    net = delta.network_mutations[0]
    assert "sk-live-secret-12345" not in net.url
    assert "PHANTOM_KEY" in net.url
    assert "sk-live-secret-12345" not in net.headers["Authorization"]
    assert "PHANTOM_KEY" in net.headers["Authorization"]
    assert b"sk-live-secret-12345" not in net.request_body
    assert b"PHANTOM_KEY" in net.request_body
    assert b"sk-live-secret-12345" not in net.response_body
    assert b"PHANTOM_KEY" in net.response_body

    assert "sk-live-secret-12345" not in delta.decision.reason
    assert "PHANTOM_KEY" in delta.decision.reason


@pytest.mark.asyncio
async def test_sandboxed_mcp_tool_decorator_and_call():
    """A mocked boundary must never cause tool code to execute on the host."""
    client = DryExecClient(ffi_module=MockFFI())
    calls = []

    @sandboxed_mcp_tool(client=client, auto_commit=True)
    async def add_numbers(a: int, b: int) -> int:
        calls.append((a, b))
        return a + b

    result: SandboxedToolResult = await add_numbers(10, 32)
    assert result.result is None
    assert result.committed is False
    assert calls == []
    assert result.delta.decision.choice == Choice.ALLOWED

    # Direct functional call test
    async def sample_tool(msg: str) -> str:
        return f"Hello, {msg}"

    res = await sandboxed_mcp_call(
        sample_tool, "PostHog", client=client, auto_commit=False
    )
    assert res.result is None
    assert res.committed is False


@pytest.mark.asyncio
async def test_reproduce_standalone_trial_rollback_wipes_scratch():
    """Reproducer: Standalone trial without workspace_dir must allocate scratch and wipe it on rollback."""
    client = DryExecClient(ffi_module=MockFFI())
    env = Environment()
    action = Action(
        action_id="act_standalone",
        target_resource="fs",
        mutation_type="execute",
    )

    trial = await client.run_trial(env, action, workspace_dir=None)
    assert trial._scratch_dir is not None
    scratch_path = Path(trial._scratch_dir)
    assert scratch_path.exists()

    await trial.rollback()
    assert trial.is_rolled_back
    assert not scratch_path.exists()


@pytest.mark.asyncio
async def test_reproduce_mcp_tool_mutation_contained_in_scratch():
    """Reproducer: Tool mutations must be isolated in scratch and not leak to workspace on rollback."""
    with tempfile.TemporaryDirectory() as target_ws:
        ws_file = Path(target_ws) / "leaked.txt"

        def mutating_tool():
            # Attempts to write to target workspace or current dir
            Path("leaked.txt").write_text("should be rolled back")
            return {"status": "wrote"}

        client = DryExecClient()
        res = await sandboxed_mcp_call(
            mutating_tool,
            workspace_dir=target_ws,
            auto_commit=False,
            client=client,
        )

        assert res.result == {"status": "wrote"}
        assert res.committed is False
        assert not ws_file.exists()
        assert not Path("leaked.txt").exists()


def test_trial_source_resolution_rejects_escape_paths(tmp_path):
    """Only relative in-scratch paths may be applied to the durable workspace."""
    scratch = (tmp_path / "scratch").resolve()
    scratch.mkdir()

    assert Trial._relative_source("/etc/passwd", scratch) is None
    assert Trial._relative_source("../escape.txt", scratch) is None
    assert Trial._relative_source("nested/file.txt", scratch) == Path("nested/file.txt")


@pytest.mark.asyncio
async def test_clone_workspace_re_raises_overlap_error(tmp_path):
    """Destination overlapping source tree must fail closed rather than falling back to copytree."""
    source = tmp_path / "source"
    source.mkdir()
    (source / "file.txt").write_text("data")
    destination = source / "nested"

    client = DryExecClient()
    with pytest.raises(Exception, match="overlaps the source tree"):
        await client._clone_workspace(str(source), str(destination))


@pytest.mark.asyncio
async def test_trial_missing_source_preserves_existing_target(tmp_path):
    """A stale delta entry must not delete a target when its scratch source vanished."""
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    target = workspace / "state.txt"
    target.write_text("original")

    scratch = tmp_path / "scratch"
    scratch.mkdir()

    delta = _mock_delta(
        mutations=[
            FsMutation(mutation_type="modified", path="state.txt"),
        ]
    )
    trial = Trial(
        delta=delta,
        environment=Environment(),
        action=Action(
            action_id="stale_source",
            target_resource="state.txt",
            mutation_type="modify",
        ),
        scratch_dir=str(scratch),
        workspace_dir=str(workspace),
    )

    await trial.commit()
    assert target.read_text() == "original"


@pytest.mark.asyncio
async def test_trial_commit_rejects_symlink_escape_and_rolls_back(tmp_path):
    """A workspace symlink must not let a commit write outside the target tree."""
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    outside = tmp_path / "outside"
    outside.mkdir()
    (workspace / "link").symlink_to(outside, target_is_directory=True)

    applied = workspace / "a.txt"
    applied.write_text("original")

    scratch = tmp_path / "scratch"
    scratch.mkdir()
    (scratch / "a.txt").write_text("changed")
    (scratch / "link").mkdir()
    (scratch / "link" / "leak.txt").write_text("escaped")

    delta = _mock_delta(
        mutations=[
            FsMutation(mutation_type="modified", path="a.txt"),
            FsMutation(mutation_type="created", path="link/leak.txt"),
        ]
    )
    trial = Trial(
        delta=delta,
        environment=Environment(),
        action=Action(
            action_id="symlink_escape",
            target_resource="a.txt",
            mutation_type="modify",
        ),
        scratch_dir=str(scratch),
        workspace_dir=str(workspace),
    )

    with pytest.raises(RuntimeError, match="outside workspace"):
        await trial.commit()

    assert not trial.is_committed
    assert applied.read_text() == "original"
    assert not (outside / "leak.txt").exists()


def test_ffi_rejects_invalid_action_without_touching_boundary():
    """Direct FFI callers must hit the native schema gate before execution."""
    from dry_exec.client import _dry_exec_ffi

    if _dry_exec_ffi is None:
        pytest.skip("Native FFI primitive is not loaded")

    with pytest.raises(_dry_exec_ffi.SchemaViolationError, match="action_id"):
        _dry_exec_ffi.execute_isolated_action(
            '{"action_id": ""}',
            1024,
            "/tmp/dry_exec_invalid",
            False,
            None,
            None,
            None,
            None,
            None,
            None,
        )

    with pytest.raises(_dry_exec_ffi.SchemaViolationError) as exc:
        _dry_exec_ffi.execute_isolated_action(
            '{"action_id":"a","target_resource":"forbidden","mutation_type":"m","payload":{}}',
            1024,
            "/tmp/dry_exec_invalid",
            False,
            None,
            None,
            None,
            None,
            ["allowed"],
            None,
        )
    assert exc.value.invalid_target == "forbidden"


def test_security_level_fails_closed_before_ffi():
    """An unmet isolation strength must refuse execution instead of silently degrading."""
    from dry_exec import IsolationSetupError, SecurityLevel

    client = DryExecClient(ffi_module=MockFFI())
    environment = Environment(minimum_security_level=SecurityLevel.MICROVM)
    action = Action(
        action_id="security_level",
        target_resource="db",
        mutation_type="execute",
    )

    with pytest.raises(IsolationSetupError, match="provides only"):
        asyncio_run(client.execute_ephemeral_action(environment, action))


def asyncio_run(coro):
    import asyncio

    return asyncio.new_event_loop().run_until_complete(coro)


def test_commit_applies_captured_delta_without_host_reexecution(monkeypatch, tmp_path):
    """The commit path must use the captured delta, never replay the callable on the host."""
    import dry_exec

    executions = []

    def target() -> dict:
        executions.append("host")
        return {"ok": True}

    scratch = tmp_path / "scratch"
    scratch.mkdir()
    workspace = tmp_path / "workspace"
    workspace.mkdir()

    async def mock_run_trial(self, environment, action, workspace_dir=None, **kwargs):
        return Trial(
            delta=_mock_delta(),
            environment=environment,
            action=action,
            scratch_dir=str(scratch),
            workspace_dir=str(workspace_dir),
        )

    async def boom(*args, **kwargs):
        raise AssertionError("host re-execution must not occur")

    monkeypatch.setattr(DryExecClient, "run_trial", mock_run_trial)
    monkeypatch.setattr(DryExecClient, "execute_ephemeral_action", boom)

    dry_exec.run(target, commit=True, workspace_dir=str(workspace))

    assert executions == []


@pytest.mark.asyncio
async def test_trial_commit_preserves_created_symlink(tmp_path):
    """A symlink created in scratch is applied as a link, not followed and copied."""
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    scratch = tmp_path / "scratch"
    scratch.mkdir()
    (scratch / "link").symlink_to("/etc/passwd")

    trial = Trial(
        delta=_mock_delta(mutations=[FsMutation(mutation_type="created", path="link")]),
        environment=Environment(),
        action=Action(
            action_id="symlink_create",
            target_resource="link",
            mutation_type="create",
        ),
        scratch_dir=str(scratch),
        workspace_dir=str(workspace),
    )

    await trial.commit()

    applied = workspace / "link"
    assert applied.is_symlink()
    assert applied.readlink() == Path("/etc/passwd")


@pytest.mark.asyncio
async def test_run_trial_rejects_missing_workspace(tmp_path):
    """An explicit workspace must exist; silently running elsewhere hides caller mistakes."""
    client = DryExecClient(ffi_module=MockFFI())
    action = Action(
        action_id="missing_workspace",
        target_resource="fs",
        mutation_type="execute",
    )

    with pytest.raises(FileNotFoundError, match="workspace_dir"):
        await client.run_trial(
            Environment(), action, workspace_dir=str(tmp_path / "missing")
        )
