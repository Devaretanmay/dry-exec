# Preventing API leakage during agent runs

Register only deterministic endpoints needed by a trial. The isolated command receives proxy settings; unregistered routes become schema breaches.

```python
from dry_exec import Action, DryExecClient, Environment, MockResponse

env = Environment(
    name="payments-trial",
    allowed_mutation_targets={"payment-preview"},
    allowed_api_endpoints={
        "GET https://api.example.test/quote": MockResponse(
            body='{"amount": 1250, "currency": "USD"}'
        )
    },
)
action = Action(
    action_id="quote-001",
    target_resource="payment-preview",
    mutation_type="execute",
    payload={"command": ["python3", "tools/fetch_quote.py"]},
)

delta = await DryExecClient().execute_ephemeral_action(env, action)
assert delta.schema_breaches == 0
for request in delta.network_mutations:
    print(request.method, request.url, request.response_status)
```

HTTP and HTTPS mock routing are supported. HTTPS uses an ephemeral per-host certificate; production clients should inject and trust a trial CA instead of disabling verification.
