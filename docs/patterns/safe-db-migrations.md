# Safe database migrations

Use `dry-exec` to exercise migration logic against an ephemeral boundary before applying it to a durable database.

```python
from dry_exec import Action, DryExecClient, Environment

env = Environment(
    name="migration-review",
    allowed_mutation_targets={"schema"},
    allowed_filesystem_roots=["/tmp/dry_exec_ephemeral"],
)
action = Action(
    action_id="add-index-v1",
    target_resource="schema",
    mutation_type="execute",
    payload={"command": ["python3", "migrations/add_index.py"]},
)

delta = await DryExecClient().execute_ephemeral_action(env, action)
if delta.exit_code != 0:
    raise RuntimeError(delta.stderr.decode())
for mutation in delta.fs_mutations:
    print(mutation.mutation_type, mutation.path)
```

Review `exit_code`, filesystem mutations, and network mutations before applying the same migration to a durable target. dry-exec does not replace database backups, transaction strategy, or migration review.
