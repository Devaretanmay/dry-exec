"""OpenTelemetry and structured logging integration."""

import json
import logging
from contextlib import contextmanager
from typing import Any, Dict, Iterator, Optional
from pydantic import BaseModel, Field
from dry_exec.exceptions import SchemaViolationError, SyscallBoundaryError
from dry_exec.models import StateDelta
from dry_exec.schemas import Action, Environment

try:
    from opentelemetry import trace
    from opentelemetry.trace import Status, StatusCode
    OTEL_AVAILABLE = True
except ImportError:
    trace = None
    Status = None
    StatusCode = None
    OTEL_AVAILABLE = False


class DeltaMetrics(BaseModel):
    duration_nanos: int
    duration_ms: float
    total_bytes_mutated: int
    memory_page_count: int
    fs_mutation_count: int
    network_mutation_count: int


class DeltaReceipt(BaseModel):
    telemetry_type: str = "state_delta_receipt"
    service: str
    trial_id: int
    committed: bool
    environment: Dict[str, Any]
    action: Dict[str, Any]
    delta_metrics: DeltaMetrics
    network_requests: list[Dict[str, Any]] = Field(default_factory=list)
    fs_mutations: list[Dict[str, Any]] = Field(default_factory=list)


class TelemetryExporter:
    """Exports structured execution receipts and OpenTelemetry spans."""

    def __init__(
        self,
        service_name: str = "dry-exec",
        logger: Optional[logging.Logger] = None,
        emit_json_logs: bool = True,
    ):
        self.service_name = service_name
        self.logger = logger or logging.getLogger("dry_exec.telemetry")
        self.emit_json_logs = emit_json_logs
        self.tracer = trace.get_tracer(service_name) if OTEL_AVAILABLE else None

    def export_delta_receipt(
        self,
        environment: Environment,
        action: Action,
        delta: StateDelta,
        trial_id: int = 1,
        committed: bool = False,
    ) -> Dict[str, Any]:
        """Serializes StateDelta telemetry via Pydantic model_dump."""
        receipt_model = DeltaReceipt(
            service=self.service_name,
            trial_id=trial_id,
            committed=committed,
            environment={
                "name": environment.name,
                "memory_limit_bytes": environment.memory_limit_bytes,
                "allowed_mutation_targets": list(environment.allowed_mutation_targets),
            },
            action=action.model_dump(),
            delta_metrics=DeltaMetrics(
                duration_nanos=delta.duration_nanos,
                duration_ms=delta.duration_nanos / 1_000_000.0,
                total_bytes_mutated=delta.total_bytes_mutated,
                memory_page_count=len(delta.memory_mutations),
                fs_mutation_count=len(delta.fs_mutations),
                network_mutation_count=len(delta.network_mutations),
            ),
            network_requests=[
                {"method": r.method, "url": r.url, "response_status": r.response_status}
                for r in delta.network_mutations
            ],
            fs_mutations=[
                {"path": m.path, "mutation_type": m.mutation_type}
                for m in delta.fs_mutations
            ],
        )
        receipt_dict = receipt_model.model_dump(mode="json")
        if self.emit_json_logs:
            self.logger.info(json.dumps(receipt_dict))
        return receipt_dict

    def export_error(
        self,
        environment: Environment,
        action: Action,
        error: Exception,
        trial_id: int = 1,
    ) -> Dict[str, Any]:
        """Logs boundary violations as structured JSON."""
        payload: Dict[str, Any] = {
            "telemetry_type": "boundary_error",
            "service": self.service_name,
            "trial_id": trial_id,
            "environment_name": environment.name,
            "action_id": action.action_id,
            "error_type": type(error).__name__,
            "message": str(error),
        }
        if isinstance(error, SyscallBoundaryError):
            payload["syscall_nr"] = error.syscall_nr
            payload["instruction_pointer"] = f"0x{error.instruction_pointer:x}"
        elif isinstance(error, SchemaViolationError):
            payload["invalid_target"] = error.invalid_target
            payload["violation_type"] = error.violation_type

        if self.emit_json_logs:
            self.logger.error(json.dumps(payload))
        return payload

    @contextmanager
    def trace_ephemeral_action(
        self,
        environment: Environment,
        action: Action,
    ) -> Iterator[Optional[Any]]:
        """Wraps execution in an OpenTelemetry span with boundary attributes."""
        if not OTEL_AVAILABLE or self.tracer is None:
            yield None
            return

        with self.tracer.start_as_current_span("dry_exec.execute_ephemeral_action") as span:
            span.set_attribute("dry_exec.environment.name", environment.name)
            span.set_attribute("dry_exec.environment.memory_limit", environment.memory_limit_bytes)
            span.set_attribute("dry_exec.action.id", action.action_id)
            span.set_attribute("dry_exec.action.target", action.target_resource)
            span.set_attribute("dry_exec.action.mutation_type", action.mutation_type)
            try:
                yield span
            except SyscallBoundaryError as exc:
                span.set_status(Status(StatusCode.ERROR, f"Syscall Boundary Interception: nr={exc.syscall_nr}"))
                span.set_attribute("dry_exec.violation.syscall_nr", exc.syscall_nr)
                span.set_attribute("dry_exec.violation.instruction_pointer", f"0x{exc.instruction_pointer:x}")
                raise
            except SchemaViolationError as exc:
                span.set_status(Status(StatusCode.ERROR, f"Schema Boundary Rejection: {exc.violation_type}"))
                span.set_attribute("dry_exec.violation.invalid_target", exc.invalid_target)
                span.set_attribute("dry_exec.violation.type", exc.violation_type)
                raise
            except Exception as exc:
                span.set_status(Status(StatusCode.ERROR, str(exc)))
                raise
