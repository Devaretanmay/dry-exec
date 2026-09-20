"""Type-safe async client for the dry-exec ephemeral execution boundary."""

import asyncio
import json
from typing import Any, Dict, Optional, Tuple
from dry_exec.exceptions import (
    IsolationSetupError,
    SchemaViolationError,
    StateDeltaComputationError,
    SyscallBoundaryError,
)
from dry_exec.models import ByteDelta, FsMutation, InterceptedRequest, PageMutation, StateDelta
from dry_exec.schemas import Action, Environment

try:
    import _dry_exec_ffi  # type: ignore
except ImportError:
    try:
        from dry_exec import _dry_exec_ffi  # type: ignore
    except ImportError:
        _dry_exec_ffi = None


class DryExecClient:
    """High-performance client providing ephemeral execution primitives for autonomous execution loops."""

    def __init__(self, ffi_module: Any = None):
        self._ffi = ffi_module if ffi_module is not None else _dry_exec_ffi

    async def execute_ephemeral_action(
        self,
        environment: Environment,
        action: Action,
        trigger_blocked_syscall: bool = False,
        request_to_trigger: Optional[Tuple[str, str, str]] = None,
    ) -> StateDelta:
        """Executes a proposed state-mutation action within the isolated kernel boundary.
        
        Enforces synchronous schema validation before crossing into the FFI execution layer.
        """
        # 1. Synchronous schema validation gatekeeping (pre-execution boundary)
        environment.validate_action(action)

        if self._ffi is None:
            raise IsolationSetupError(
                "Rust FFI primitive _dry_exec_ffi is not loaded. "
                "Ensure execution occurs within the Linux container verification harness."
            )

        # 2. Serialize schema-driven mock endpoints for transparent network proxy
        mock_endpoints_list = []
        for route_key, mock_resp in environment.allowed_api_endpoints.items():
            parts = route_key.strip().split(maxsplit=1)
            method = parts[0].upper() if len(parts) > 0 else "GET"
            path = parts[1] if len(parts) > 1 else "/"
            mock_endpoints_list.append({
                "method": method,
                "path": path,
                "status_code": mock_resp.status_code,
                "headers": mock_resp.headers,
                "body": mock_resp.body,
            })

        mock_endpoints_json = json.dumps(mock_endpoints_list) if mock_endpoints_list else None

        action_payload_json = action.model_dump_json()
        tmpfs_path = (
            environment.allowed_filesystem_roots[0]
            if environment.allowed_filesystem_roots
            else "/tmp/dry_exec_ephemeral"
        )

        # 3. Asynchronously dispatch blocking kernel operations to maintain event loop liveness
        try:
            raw_delta: Dict[str, Any] = await asyncio.to_thread(
                self._ffi.execute_isolated_action,
                action_payload_json,
                environment.memory_limit_bytes,
                tmpfs_path,
                trigger_blocked_syscall,
                mock_endpoints_json,
                request_to_trigger,
            )
        except Exception as exc:
            exc_type = type(exc).__name__
            if exc_type == "SyscallBoundaryError" or hasattr(exc, "syscall_nr"):
                syscall_nr = getattr(exc, "syscall_nr", 0)
                ip = getattr(exc, "instruction_pointer", 0)
                raise SyscallBoundaryError(str(exc), syscall_nr=syscall_nr, instruction_pointer=ip) from exc
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
            total_bytes_mutated=raw_delta.get("total_bytes_mutated", 0),
            duration_nanos=raw_delta.get("duration_nanos", 0),
        )
