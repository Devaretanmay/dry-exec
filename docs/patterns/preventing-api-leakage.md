# Preventing API leakage during agent runs

When an autonomous AI agent explores actions, you never want it making live external API calls: it might trigger real payments, deplete API rate limits, or send emails prematurely.

`dry-exec` intercepts all outbound network requests through an in-memory loopback proxy. Only routes explicitly registered in your `Environment` return deterministic mock responses. Any unlisted route is rejected and recorded as a schema breach.

## Quick example

Intercept outbound HTTP/HTTPS calls and supply deterministic mock data:

```python
import asyncio
from dry_exec import Action, DryExecClient, Environment, MockResponse

async def run_payment_trial():
    # 1. Register only permitted endpoints with deterministic mock responses
    env = Environment(
        name="payments-trial",
        allowed_mutation_targets={"payment-preview"},
        allowed_api_endpoints={
            "GET https://api.stripe.com/v1/prices": MockResponse(
                status_code=200,
                body='{"data": [{"id": "price_123", "unit_amount": 2000}]}',
            )
        },
    )

    # 2. Propose the action
    action = Action(
        action_id="quote-001",
        target_resource="payment-preview",
        mutation_type="execute",
        payload={"command": ["python3", "tools/fetch_quote.py"]},
    )

    # 3. Execute behind the transparent proxy boundary
    delta = await DryExecClient().execute_ephemeral_action(env, action)

    # 4. Verify zero unexpected network calls occurred
    assert delta.schema_breaches == 0, f"Leaked {delta.schema_breaches} unauthorized API calls!"
    for req in delta.network_mutations:
        print(f"Intercepted: {req.method} {req.url} -> HTTP {req.response_status}")

asyncio.run(run_payment_trial())
```

## How mock interception works

1. **Loopback rerouting**: On Linux, `CLONE_NEWNET` leaves network interfaces unconfigured except for loopback, forcing traffic through the local proxy. On macOS, Seatbelt and proxy environment variables redirect outbound requests.
2. **HTTPS handling**: HTTPS requests via HTTP `CONNECT` tunneling receive an ephemeral per-host certificate generated dynamically in memory.
3. **Deterministic auditing**: Every intercepted request is captured in `delta.network_mutations`, including request method, path, headers, request body, and mock response status.
4. **Schema breach detection**: If your code calls an unregistered endpoint (e.g., `POST https://api.sendgrid.com/v3/mail/send`), the proxy returns HTTP 400 Bad Request and increments `delta.schema_breaches`.
