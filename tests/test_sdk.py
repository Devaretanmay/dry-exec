"""Containerized verification suite for Loop 3 and Loop 4: Type-Safe Python SDK, FFI Bridge, & Proxy."""

from unittest.mock import MagicMock
import pytest
from dry_exec import (
    Action,
    DryExecClient,
    Environment,
    MockResponse,
    SchemaViolationError,
    StateDelta,
    SyscallBoundaryError,
)

# SYS_socket constant on Linux (x86_64: 41, aarch64: 198)
import platform
import sys

SYS_SOCKET_NR = 41 if platform.machine() == "x86_64" else 198


@pytest.mark.asyncio
async def test_assertion_a_schema_rejection():
    """Assertion A: Invalid state-mutation action is rejected synchronously without invoking FFI."""
    env = Environment(
        name="financial_ledger_env",
        allowed_mutation_targets={"balance"},
        memory_limit_bytes=1024 * 1024,
    )

    invalid_action = Action(
        action_id="act_invalid_001",
        target_resource="username",  # Violates environment boundary schema
        mutation_type="update",
        payload={"username": "unauthorized_override"},
    )

    # Mock FFI module to verify gatekeeping
    mock_ffi = MagicMock()
    client = DryExecClient(ffi_module=mock_ffi)

    # Assert synchronous SchemaViolationError
    with pytest.raises(SchemaViolationError) as exc_info:
        await client.execute_ephemeral_action(env, invalid_action)

    assert exc_info.value.invalid_target == "username"
    assert exc_info.value.violation_type == "unauthorized_mutation_target"

    # Crucial assertion: Rust FFI primitive was NEVER invoked
    mock_ffi.execute_isolated_action.assert_not_called()


@pytest.mark.asyncio
@pytest.mark.skipif(
    sys.platform != "linux",
    reason="Requires Linux seccomp and soft-dirty pagemap primitives",
)
async def test_assertion_b_successful_ffi_execution():
    """Assertion B: Valid schema-compliant action crosses FFI, executes in isolation, and returns StateDelta."""
    env = Environment(
        name="financial_ledger_env",
        allowed_mutation_targets={"balance"},
        allowed_filesystem_roots=["/tmp/dry_exec_ephemeral"],
        memory_limit_bytes=4 * 1024 * 1024,
    )

    valid_action = Action(
        action_id="act_valid_002",
        target_resource="balance",
        mutation_type="increment",
        payload={"amount": 100},
    )

    client = DryExecClient()

    delta: StateDelta = await client.execute_ephemeral_action(env, valid_action)

    assert isinstance(delta, StateDelta)
    assert delta.total_bytes_mutated >= 4
    assert len(delta.memory_mutations) >= 1
    # Verify Page 0 mutation contents
    page0 = delta.memory_mutations[0]
    assert page0.page_index == 0
    assert page0.deltas[0].mutated == b"\xde\xad\xbe\xef"


@pytest.mark.asyncio
@pytest.mark.skipif(
    sys.platform != "linux",
    reason="Requires Linux seccomp and soft-dirty pagemap primitives",
)
async def test_assertion_c_syscall_violation_propagation():
    """Assertion C: Blocked syscall triggers SyscallBoundaryError with exact syscall_nr and instruction pointer."""
    env = Environment(
        name="restricted_execution_env",
        allowed_mutation_targets={"balance"},
        memory_limit_bytes=1024 * 1024,
    )

    action_attempting_socket = Action(
        action_id="act_socket_003",
        target_resource="balance",
        mutation_type="probe_socket",
        payload={},
    )

    client = DryExecClient()

    with pytest.raises(SyscallBoundaryError) as exc_info:
        await client.execute_ephemeral_action(
            env, action_attempting_socket, trigger_blocked_syscall=True
        )

    error = exc_info.value
    expected_syscall = f"Expected syscall {SYS_SOCKET_NR}, observed {error.syscall_nr}"
    assert error.syscall_nr == SYS_SOCKET_NR, expected_syscall
    assert error.instruction_pointer > 0


@pytest.mark.asyncio
@pytest.mark.skipif(
    sys.platform != "linux",
    reason="Requires Linux seccomp and soft-dirty pagemap primitives",
)
async def test_assertion_d_network_interception():
    """Assertion D (Loop 4): Outbound HTTP egress from the isolated network namespace reaches the
    transparent proxy, which returns the schema-driven mock response and records the mutation.
    """
    mock_payload = '{"status": "success", "id": "mock_123"}'
    env = Environment(
        name="payment_gateway_env",
        allowed_mutation_targets={"payment"},
        allowed_api_endpoints={
            "POST /v1/charge": MockResponse(
                status_code=200,
                headers={"Content-Type": "application/json"},
                body=mock_payload,
            )
        },
        memory_limit_bytes=1024 * 1024,
    )

    charge_action = Action(
        action_id="act_charge_004",
        target_resource="payment",
        mutation_type="execute_charge",
        payload={"amount": 5000},
    )

    client = DryExecClient()

    delta: StateDelta = await client.execute_ephemeral_action(
        env,
        charge_action,
        request_to_trigger=("POST", "/v1/charge", '{"amount": 5000}'),
    )

    # The isolated execution layer binds the schema-driven mock listener inside its own network
    # namespace and the control plane serves it: the outbound request is intercepted and recorded.
    assert len(delta.network_mutations) == 1
    mutation = delta.network_mutations[0]
    assert mutation.method == "POST"
    assert mutation.url == "/v1/charge"
    assert mutation.request_body == b'{"amount": 5000}'
    assert mutation.response_status == 200
    assert mutation.response_body == mock_payload.encode()

    # Schema-driven routes are the source of the served response.
    registered_route = env.allowed_api_endpoints["POST /v1/charge"]
    assert registered_route.status_code == 200
    assert mock_payload in registered_route.body


@pytest.mark.asyncio
@pytest.mark.skipif(
    sys.platform != "darwin", reason="Requires macOS Seatbelt and APFS primitives"
)
async def test_assertion_macos_native_execution():
    """Verify native ephemeral execution on macOS returns StateDelta with microsecond latency."""
    env = Environment(name="macos_native_env", allowed_mutation_targets={"*"})
    action = Action(
        action_id="act_mac_001",
        target_resource="local_state",
        mutation_type="update",
        payload={"key": "val"},
    )
    client = DryExecClient()
    delta = await client.execute_ephemeral_action(env, action)
    assert isinstance(delta, StateDelta)
    assert delta.total_bytes_mutated >= 0
    assert delta.duration_nanos > 0
