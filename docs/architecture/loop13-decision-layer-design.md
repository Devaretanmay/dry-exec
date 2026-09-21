# Loop 13 Architecture: System-One Decision Layer

## 1. System Overview

Loop 13 replaces chatty text-based validation of an ephemeral state mutation with a deterministic,
calibrated routing primitive evaluated in microseconds. The decision layer sits between state delta
generation and the Python SDK's return to the caller. It does not read program text, it does not
invoke a language model: it evaluates the mathematical receipt of the mutation against the typed
thresholds of the environment.

```text
+-------------------------------------------------------------------------+
|                    Python SDK / `dex` CLI / Agent loop                  |
|   routing only: escalation halts control flow or forces an explicit      |
|   --commit; no scoring logic is duplicated outside the execution layer   |
+---------------------------------+---------------------------------------+
                                  | PyO3 FFI (single crossing)
                                  v
+-------------------------------------------------------------------------+
|                  Rust Control Plane (dry-exec-core)                     |
|                                                                         |
|  1. Ephemeral Execution (namespaces / Seatbelt)                         |
|  2. State Delta Engine (pagemap soft-dirty / APFS CoW)                  |
|         |  produces StateDelta { bytes, inodes, intercepted calls,      |
|         |                        schema_breaches }                     |
|         v                                                               |
|  3. SYSTEM-ONE DECISION LAYER (decision/)                               |
|     +-------------------------------------------------------------+     |
|     | Input A: DeltaSummary  (Copy scalar projection, O(1))       |     |
|     | Input B: Environment   (calibrated thresholds)              |     |
|     |                                                             |     |
|     | risk_score = 0.5 * bytes_ratio + 0.5 * network_ratio        |     |
|     | choice     = Violated | Blocked | Allowed                   |     |
|     | noul       = Escalate | AutoCommit                           |     |
|     +-------------------------------------------------------------+     |
|                                                                         |
|  Output: DecisionReceipt { choice, risk_score, noul_trigger, reason }    |
+-------------------------------------------------------------------------+
```

## 2. Module Specification (`crates/dry-exec-core/src/decision/`)

| File | Contents |
| --- | --- |
| `primitives.rs` | `Choice { Allowed, Blocked, Violated }`, `Score(f32)` clamped into `[0.0, 1.0]`, `Noul { AutoCommit, Escalate }`, `DecisionReceipt` |
| `environment.rs` | `DeltaSummary` (the sole scorer input) and `Environment` (calibrated thresholds) |
| `mod.rs` | `evaluate(&DeltaSummary, &Environment) -> DecisionReceipt` and the calibrated weights |

### 2.1 Calibrated Risk Metric

```
bytes_ratio   = total_bytes_mutated / max_mutated_bytes
network_ratio = network_calls       / max_network_calls
risk_score    = Score::new(0.5 * bytes_ratio + 0.5 * network_ratio)
```

`Score::new` clamps into the calibrated range and saturates non-finite input to `1.0`, so an
unevaluable ratio routes as maximum risk rather than as an unconstrained value. A zero ceiling is
an exhausted budget: no mutation scores `0.0`, any mutation saturates, and the boundary never
divides by zero.

### 2.2 Routing

| Condition | `Choice` | `Noul` |
| --- | --- | --- |
| `bytes_ratio > 1.0` or `network_ratio > 1.0` | `Violated` | `Escalate` |
| `schema_breaches > 0` | `Blocked` | `Escalate` |
| otherwise, `risk_score > max_risk_threshold` | `Allowed` | `Escalate` |
| otherwise | `Allowed` | `AutoCommit` |

Precedence is `Violated` > `Blocked` > `Allowed`. `reason` is one of four interpolated system
strings; no explanation is generated.

## 3. The O(1) Constraint

The score is computed from `DeltaSummary`, a `Copy` projection of the state delta that holds counts
and totals only. The mutation vectors are therefore **not in scope** inside the scored region: the
complexity bound is a property the compiler enforces rather than a convention reviewers police.
`DeltaSummary::from_state_delta` reads only lengths and scalar totals, which is O(1) against the
size of the state.

`Choice::Blocked` needs a categorical signal that a scalar summary can carry. A mutation target
outside the permitted set is rejected synchronously *before* execution and never produces a state
delta, so the observable categorical refusal is the transparent proxy refusing a route outside the
registered schema. That count is precomputed once at delta-generation time
(`StateDelta::schema_breaches`) where the knowledge already exists, keeping the decision layer O(1)
without an $O(N)$ scan of `network_mutations`.

## 4. FFI Seam

The receipt is computed in the existing `execute_isolated_action` crossing, in the dict-assembly
path shared by the Linux and macOS backends, and returned under `"decision"`:

```
decision = { choice, risk_score, noul_trigger, reason }
```

A second `evaluate_decision(delta, thresholds)` entry point was rejected: re-marshalling every
mutation vector across the FFI to return four scalars is precisely the O(N) overhead this layer
exists to remove.

## 5. Python Integration Map

| Module | Responsibility |
| --- | --- |
| `models.py` | `Choice`, `Noul`, `DecisionReceipt`, and `StateDelta.decision` |
| `schemas.py` | calibrated `Environment` thresholds: `max_mutated_bytes`, `max_network_calls`, `max_risk_threshold` |
| `exceptions.py` | `DecisionEscalationError` carrying the choice, calibrated score, and system reason |
| `decision.py` | `parse_decision`, `is_escalated`, `enforce_escalation` — mapping and routing only |
| `client.py` | passes thresholds, attaches the receipt to the state delta, emits decision telemetry |
| `observability.py` | renders the decision receipt and the escalation panel |
| `cli.py` | escalation clears only through an explicit `--commit`; otherwise exit code `4` |
| `agent.py` | the autonomous execution loop halts on escalation unless `force=True` |

Exit code `4` is distinct from `1` (configuration), `2` (boundary violation), and `3`
(execution-layer error), so CI control flow can separate intentional escalation from failure. An
interactive confirmation cannot clear escalation: only the explicit flag does.

## 6. Verification

- `cargo test --lib` gates the primitives, the routing table, the latency bound, and latency
  invariance across state size.
- `benches/decision_bench.rs` reports `evaluate` latency for a 100MB state delta alongside an
  empty one.
- `tests/test_decision.py` gates the receipt mapping, escalation routing, CLI/Agent gates, and
  end-to-end routing through the real execution layer.
- Container harness Phase 6 executes the decision suite against the kernel boundary.
