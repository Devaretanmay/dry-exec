"""Production-ready example: Deterministic API mutation exploration via transparent network proxy."""

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
    """Demonstrates external API state exploration with zero network egress."""
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

    # 3. Execute in ephemeral isolation; outbound HTTP is intercepted by transparent proxy
    delta: StateDelta = await client.execute_ephemeral_action(
        env,
        charge_action,
        request_to_trigger=("POST", "/v1/charges", '{"amount": 2500, "customer": "cus_1234"}'),
    )

    # 4. Render observability receipt
    logger.render_delta(delta)

    # 5. Assert deterministic boundary behavior
    assert len(delta.network_mutations) == 1
    net_record = delta.network_mutations[0]
    assert net_record.method == "POST"
    assert net_record.url == "/v1/charges"
    assert net_record.response_status == 200
    assert b"ch_mock_9988" in net_record.response_body

    logger.render_commit_prompt(confirmed=True)
    print("\n[SUCCESS] Deterministic external API interception workflow verified.")


if __name__ == "__main__":
    asyncio.run(run_api_payment_exploration())
