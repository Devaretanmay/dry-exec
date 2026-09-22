"""Async client for dry-exec ephemeral kernel isolation."""

import asyncio
from typing import Any, Dict, Optional, Tuple
from dry_exec.decision import parse_decision
from dry_exec.exceptions import (
    IsolationSetupError,
    StateDeltaComputationError,
    SyscallBoundaryError,
)
from dry_exec.models import (
    ByteDelta,
    FsMutation,
    InterceptedRequest,
    PageMutation,
    StateDelta,
)
from dry_exec.schemas import Action, Environment
from dry_exec.telemetry import TelemetryExporter

try:
    from dry_exec import _dry_exec_ffi  # type: ignore
except ImportError:
    try:
        import _dry_exec_ffi  # type: ignore
    except ImportError:
        _dry_exec_ffi = None

if _dry_exec_ffi is None:
    import importlib.util
    from pathlib import Path

    repo_root = Path(__file__).resolve().parent.parent.parent
    candidates = list(repo_root.glob("target/**/_dry_exec_ffi*.so")) + list(
        repo_root.glob("target/wheels/*.whl")
    )
    for candidate in candidates:
        if candidate.suffix == ".whl":
            import zipfile
            import tempfile

            try:
                with zipfile.ZipFile(candidate, "r") as z:
                    for name in z.namelist():
                        if name.endswith(".so"):
                            tmp_so = Path(tempfile.gettempdir()) / Path(name).name
                            tmp_so.write_bytes(z.read(name))
                            spec = importlib.util.spec_from_file_location(
                                "_dry_exec_ffi", tmp_so
                            )
                            if spec and spec.loader:
                                mod = importlib.util.module_from_spec(spec)
                                spec.loader.exec_module(mod)
                                _dry_exec_ffi = mod
                                break
            except Exception:
                pass
            if _dry_exec_ffi is not None:
                break
        else:
            try:
                spec = importlib.util.spec_from_file_location(
                    "_dry_exec_ffi", candidate
                )
                if spec and spec.loader:
                    mod = importlib.util.module_from_spec(spec)
                    spec.loader.exec_module(mod)
                    _dry_exec_ffi = mod
                    break
            except Exception:
                pass


class DryExecClient:
    """Client for dry-run execution and state delta tracking."""

    def __init__(
        self,
        ffi_module: Any = None,
        telemetry_exporter: Optional[TelemetryExporter] = None,
    ):
        self._ffi = ffi_module if ffi_module is not None else _dry_exec_ffi
        self.telemetry = telemetry_exporter or TelemetryExporter()

    async def execute_ephemeral_action(
        self,
        environment: Environment,
        action: Action,
        trigger_blocked_syscall: bool = False,
        request_to_trigger: Optional[Tuple[str, str, str]] = None,
    ) -> StateDelta:
        """Runs action within ephemeral boundary and returns computed StateDelta."""

        with self.telemetry.trace_ephemeral_action(environment, action) as span:
            delta = await self._execute_internal(
                environment,
                action,
                trigger_blocked_syscall=trigger_blocked_syscall,
                request_to_trigger=request_to_trigger,
            )
            if span is not None:
                span.set_attribute(
                    "dry_exec.delta.total_bytes_mutated", delta.total_bytes_mutated
                )
                span.set_attribute(
                    "dry_exec.delta.duration_nanos", delta.duration_nanos
                )
                span.set_attribute(
                    "dry_exec.delta.memory_pages", len(delta.memory_mutations)
                )
                span.set_attribute(
                    "dry_exec.delta.fs_mutations", len(delta.fs_mutations)
                )
                span.set_attribute(
                    "dry_exec.delta.network_requests", len(delta.network_mutations)
                )
                span.set_attribute(
                    "dry_exec.delta.schema_breaches", delta.schema_breaches
                )
                if delta.decision is not None:
                    span.set_attribute(
                        "dry_exec.decision.choice", delta.decision.choice.value
                    )
                    span.set_attribute(
                        "dry_exec.decision.noul", delta.decision.noul_trigger.value
                    )
                    span.set_attribute(
                        "dry_exec.decision.risk_score", delta.decision.risk_score
                    )
            return delta

    async def _execute_internal(
        self,
        environment: Environment,
        action: Action,
        trigger_blocked_syscall: bool = False,
        request_to_trigger: Optional[Tuple[str, str, str]] = None,
    ) -> StateDelta:
        # 1. Synchronous schema validation gatekeeping (pre-execution boundary)
        environment.validate_action(action)

        if self._ffi is None:
            raise IsolationSetupError(
                "Rust FFI primitive _dry_exec_ffi is not loaded. "
                "Ensure execution occurs within the Linux container verification harness."
            )

        # 2. Package schema-driven mock endpoints for transparent network proxy
        mock_endpoints_tuples = None
        if environment.allowed_api_endpoints:
            mock_endpoints_tuples = []
            for route_key, mock_resp in environment.allowed_api_endpoints.items():
                parts = route_key.strip().split(maxsplit=1)
                method = parts[0].upper() if len(parts) > 0 else "GET"
                path = parts[1] if len(parts) > 1 else "/"
                mock_endpoints_tuples.append(
                    (
                        method,
                        path,
                        mock_resp.status_code,
                        mock_resp.headers,
                        mock_resp.body.encode("utf-8"),
                    )
                )

        action_payload_json = action.model_dump_json()
        tmpfs_path = (
            environment.allowed_filesystem_roots[0]
            if environment.allowed_filesystem_roots
            else "/tmp/dry_exec_ephemeral"
        )

        # Calibrated thresholds evaluated by the Rust System-One decision layer in the same
        # crossing that produces the state delta.
        decision_thresholds = (
            environment.max_mutated_bytes,
            environment.max_network_calls,
            environment.max_risk_threshold,
        )

        raw_command = action.payload.get("command")
        command = None
        if isinstance(raw_command, list):
            command = [str(part) for part in raw_command]
        elif isinstance(raw_command, str) and raw_command.strip():
            command = ["/bin/sh", "-c", raw_command]

        # 3. Asynchronously dispatch blocking kernel operations to maintain event loop liveness
        try:
            raw_delta: Dict[str, Any] = await asyncio.to_thread(
                self._ffi.execute_isolated_action,
                action_payload_json,
                environment.memory_limit_bytes,
                tmpfs_path,
                trigger_blocked_syscall,
                mock_endpoints_tuples,
                request_to_trigger,
                decision_thresholds,
                command,
            )
        except Exception as exc:
            exc_type = type(exc).__name__
            if exc_type == "SyscallBoundaryError" or hasattr(exc, "syscall_nr"):
                syscall_nr = getattr(exc, "syscall_nr", 0)
                ip = getattr(exc, "instruction_pointer", 0)
                raise SyscallBoundaryError(
                    str(exc), syscall_nr=syscall_nr, instruction_pointer=ip
                ) from exc
            elif exc_type == "StateDeltaComputationError":
                raise StateDeltaComputationError(str(exc)) from exc
            elif exc_type == "IsolationSetupError":
                raise IsolationSetupError(str(exc)) from exc
            raise

        # 4. Transform raw FFI dictionary into type-safe StateDelta
        memory_mutations = []
        for pm in raw_delta.get("memory_mutations", []):
            page_deltas = [
                ByteDelta(
                    offset=d["offset"],
                    original=bytes(d["original"]),
                    mutated=bytes(d["mutated"]),
                )
                for d in pm.get("deltas", [])
            ]
            memory_mutations.append(
                PageMutation(
                    page_index=pm["page_index"],
                    page_address=pm["page_address"],
                    deltas=page_deltas,
                )
            )

        fs_mutations = []
        for fm in raw_delta.get("fs_mutations", []):
            fs_mutations.append(
                FsMutation(
                    mutation_type=fm["mutation_type"],
                    path=fm["path"],
                    mode=fm.get("mode"),
                    size=fm.get("size"),
                    deltas=[],
                )
            )

        network_mutations = []
        for nm in raw_delta.get("network_mutations", []):
            network_mutations.append(
                InterceptedRequest(
                    method=nm["method"],
                    url=nm["url"],
                    headers=nm.get("headers", {}),
                    request_body=bytes(nm.get("request_body", b"")),
                    response_status=nm.get("response_status", 200),
                    response_body=bytes(nm.get("response_body", b"")),
                )
            )

        return StateDelta(
            memory_mutations=memory_mutations,
            fs_mutations=fs_mutations,
            network_mutations=network_mutations,
            schema_breaches=raw_delta.get("schema_breaches", 0),
            total_bytes_mutated=raw_delta.get("total_bytes_mutated", 0),
            duration_nanos=raw_delta.get("duration_nanos", 0),
            stdout=bytes(raw_delta.get("stdout", b"")),
            stderr=bytes(raw_delta.get("stderr", b"")),
            exit_code=raw_delta.get("exit_code", 0),
            decision=parse_decision(raw_delta.get("decision")),
        )
