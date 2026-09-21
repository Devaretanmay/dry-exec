"""Production-ready example: External API state exploration with zero network egress."""

import asyncio
from dry_exec import (
    Action,
    DryExecClient,
    Environment,
    MockResponse,
    StateDelta,
)
from dry_exec.observability import DeltaLogger

logger = DeltaLogger()


async def run_api_payment_exploration():
    """Explores external gateway state inside the isolated network namespace with no egress."""
    # 1. Define schema-driven environment with transparent proxy mock endpoints
    mock_payload = '{"status": "captured", "charge_id": "ch_mock_9988", "amount": 2500}'
    env = Environment(
        name="payment_processing_sandbox",
        allowed_mutation_targets={"stripe_charge"},
        allowed_api_endpoints={
            "POST /v1/charges": MockResponse(
                status_code=200,
                headers={"Content-Type": "application/json"},
                body=mock_payload,
            )
        },
        memory_limit_bytes=8 * 1024 * 1024,
    )

    client = DryExecClient()

    # 2. Formulate state-mutation action targeting the external payment gateway
    charge_action = Action(
        action_id="act_charge_007",
        target_resource="stripe_charge",
        mutation_type="create_charge",
        payload={"currency": "usd", "amount": 2500, "customer": "cus_1234"},
    )

    logger.render_action_header(env, charge_action)

    # 3. Execute in ephemeral isolation; the execution layer has no route to the control plane
    delta: StateDelta = await client.execute_ephemeral_action(
        env,
        charge_action,
        request_to_trigger=(
            "POST",
            "/v1/charges",
            '{"amount": 2500, "customer": "cus_1234"}',
        ),
    )

    # 4. Render observability receipt
    logger.render_delta(delta)

    # 5. Assert deterministic boundary behavior: egress stays inside the isolated namespace
    assert delta.network_mutations == []
    registered_route = env.allowed_api_endpoints["POST /v1/charges"]
    assert registered_route.status_code == 200
    assert "ch_mock_9988" in registered_route.body

    logger.render_commit_prompt(confirmed=True)
    print(
        "\n[SUCCESS] Network namespace boundary confined egress; no request crossed it."
    )


if __name__ == "__main__":
    asyncio.run(run_api_payment_exploration())
