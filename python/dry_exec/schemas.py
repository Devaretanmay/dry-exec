"""Type-safe Pydantic V2 schemas governing the pre-execution boundary."""

from typing import Any, Dict, List, Set
from pydantic import BaseModel, ConfigDict, Field
from dry_exec.exceptions import SchemaViolationError


class MockResponse(BaseModel):
    """Deterministic mock response definition for a schema-driven API route."""

    model_config = ConfigDict(frozen=True, extra="forbid")

    status_code: int = Field(default=200, description="HTTP status code")
    headers: Dict[str, str] = Field(
        default_factory=lambda: {"Content-Type": "application/json"},
        description="HTTP response headers",
    )
    body: str = Field(
        default='{"status": "ok"}',
        description="Deterministic mock payload returned by the transparent proxy",
    )


class Action(BaseModel):
    """Proposed state-mutation action originating from the autonomous execution loop."""

    model_config = ConfigDict(frozen=True, extra="forbid")

    action_id: str = Field(description="Unique identifier for the action invocation")
    target_resource: str = Field(
        description="Designated state target (e.g., field, entity, or path)"
    )
    mutation_type: str = Field(
        description="Operation classification (e.g., set, append, delete)"
    )
    payload: Dict[str, Any] = Field(
        default_factory=dict, description="State-mutation arguments and parameters"
    )


class Environment(BaseModel):
    """Target execution boundary defining permitted mutation targets, endpoints, and resource limits."""

    model_config = ConfigDict(frozen=True, extra="forbid")

    name: str = Field(
        default="default_sandbox", description="Identifier for the target environment"
    )
    allowed_mutation_targets: Set[str] = Field(
        default_factory=lambda: {"*"},
        description="Immutable whitelist of state fields or entities permitted for mutation",
    )
    allowed_filesystem_roots: List[str] = Field(
        default_factory=lambda: ["/tmp/dry_exec_ephemeral"],
        description="Isolated ephemeral filesystem paths accessible to the execution layer",
    )
    allowed_api_endpoints: Dict[str, MockResponse] = Field(
        default_factory=dict,
        description="Schema-driven API routes permitted for network interception with deterministic responses",
    )
    memory_limit_bytes: int = Field(
        default=64 * 1024 * 1024,
        description="Upper memory allocation boundary in bytes",
    )

    def validate_action(self, action: Action) -> None:
        """Enforces synchronous schema validation before crossing the FFI boundary.

        Raises SchemaViolationError immediately if the proposed action targets an unauthorized resource.
        """
        if (
            "*" not in self.allowed_mutation_targets
            and action.target_resource not in self.allowed_mutation_targets
        ):
            raise SchemaViolationError(
                message=(
                    f"Action '{action.action_id}' targeting resource '{action.target_resource}' "
                    f"violates environment boundary for '{self.name}'. Permitted targets: "
                    f"{sorted(list(self.allowed_mutation_targets))}"
                ),
                invalid_target=action.target_resource,
                violation_type="unauthorized_mutation_target",
            )
