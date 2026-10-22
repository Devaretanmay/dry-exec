"""Async client for dry-exec ephemeral kernel isolation."""

import asyncio
import os
from pathlib import Path
import shutil
import tempfile
from typing import Any, Dict, Optional, Tuple
import uuid

from dry_exec.decision import enforce_escalation, parse_decision
from dry_exec.exceptions import (
    IsolationSetupError,
    StateDeltaComputationError,
    SyscallBoundaryError,
)
from dry_exec.models import (
    ByteDelta,
    DecisionReceipt,
    FsMutation,
    InterceptedRequest,
    PageMutation,
    StateDelta,
)
from dry_exec.schemas import Action, Environment

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


def _sanitize_credentials(
    raw_delta: Dict[str, Any], credential_masks: Dict[str, str]
) -> Dict[str, Any]:
    """Sanitizes raw FFI dictionary by masking real secrets with phantom tokens."""
    if not credential_masks:
        return raw_delta

    str_masks = [(real, phantom) for phantom, real in credential_masks.items() if real]
    byte_masks = [
        (real.encode("utf-8"), phantom.encode("utf-8"))
        for phantom, real in credential_masks.items()
        if real
    ]

    def scrub_bytes(b: bytes) -> bytes:
        if not b:
            return b
        res = b
        for real_b, phant_b in byte_masks:
            res = res.replace(real_b, phant_b)
        return res

    def scrub_str(s: str) -> str:
        if not s:
            return s
        res = s
        for real_s, phant_s in str_masks:
            res = res.replace(real_s, phant_s)
        return res

    if "stdout" in raw_delta and isinstance(raw_delta["stdout"], (bytes, bytearray)):
        raw_delta["stdout"] = scrub_bytes(bytes(raw_delta["stdout"]))
    if "stderr" in raw_delta and isinstance(raw_delta["stderr"], (bytes, bytearray)):
        raw_delta["stderr"] = scrub_bytes(bytes(raw_delta["stderr"]))

    for fm in raw_delta.get("fs_mutations", []):
        for d in fm.get("deltas", []):
            if "original" in d:
                d["original"] = scrub_bytes(bytes(d["original"]))
            if "mutated" in d:
                d["mutated"] = scrub_bytes(bytes(d["mutated"]))

    for nm in raw_delta.get("network_mutations", []):
        if "url" in nm:
            nm["url"] = scrub_str(nm["url"])
        if "request_body" in nm:
            nm["request_body"] = scrub_bytes(bytes(nm["request_body"]))
        if "response_body" in nm:
            nm["response_body"] = scrub_bytes(bytes(nm["response_body"]))
        if "headers" in nm and isinstance(nm["headers"], dict):
            nm["headers"] = {
                scrub_str(k): scrub_str(v) for k, v in nm["headers"].items()
            }

    if "decision" in raw_delta and isinstance(raw_delta["decision"], dict):
        if "reason" in raw_delta["decision"]:
            raw_delta["decision"]["reason"] = scrub_str(raw_delta["decision"]["reason"])

    return raw_delta


class Trial:
    """Ephemeral trial execution primitive supporting atomic commit and rollback."""

    def __init__(
        self,
        delta: StateDelta,
        environment: Environment,
        action: Action,
        scratch_dir: Optional[str] = None,
        workspace_dir: Optional[str] = None,
    ):
        self._delta = delta
        self._environment = environment
        self._action = action
        self._scratch_dir = scratch_dir
        self._workspace_dir = workspace_dir
        self._committed = False
        self._rolled_back = False

    @property
    def delta(self) -> StateDelta:
        """Deterministic state delta produced during the trial."""
        return self._delta

    @property
    def decision(self) -> Optional[DecisionReceipt]:
        """System-One decision receipt for this trial."""
        return self._delta.decision

    @property
    def is_committed(self) -> bool:
        """Whether this trial has been committed."""
        return self._committed

    @property
    def is_rolled_back(self) -> bool:
        """Whether this trial has been rolled back."""
        return self._rolled_back

    async def commit(self, force: bool = False) -> None:
        """Atomically applies trial mutations to the target workspace.

        Enforces the System-One escalation gate: halts with DecisionEscalationError
        if the receipt routes to escalation and force is False.
        """
        if self._committed:
            return
        if self._rolled_back:
            raise RuntimeError(
                "Cannot commit a trial that has already been rolled back."
            )

        enforce_escalation(self._delta.decision, force=force)

        if self._workspace_dir and self._scratch_dir:
            await self._apply_mutations_to_workspace()

        self._committed = True
        self._cleanup_scratch()

    async def rollback(self) -> None:
        """Discards ephemeral trial state, leaving target workspace untouched."""
        if self._rolled_back or self._committed:
            return
        self._rolled_back = True
        self._cleanup_scratch()

    def _cleanup_scratch(self) -> None:
        if self._scratch_dir and os.path.exists(self._scratch_dir):
            shutil.rmtree(self._scratch_dir, ignore_errors=True)

    async def _apply_mutations_to_workspace(self) -> None:
        if not self._workspace_dir or not self._scratch_dir:
            return

        def _sync_fs():
            ws = Path(self._workspace_dir).resolve()
            sc = Path(self._scratch_dir).resolve()

            for fm in self._delta.fs_mutations:
                candidate = Path(fm.path)
                try:
                    rel = candidate.resolve().relative_to(sc)
                except ValueError:
                    try:
                        rel = candidate.relative_to(Path(self._scratch_dir))
                    except ValueError:
                        rel = Path(str(candidate).lstrip("/"))

                target_path = ws / rel
                source_path = sc / rel

                if fm.mutation_type in ("created", "modified"):
                    if source_path.exists():
                        target_path.parent.mkdir(parents=True, exist_ok=True)
                        if source_path.is_file():
                            tmp_target = target_path.with_name(
                                f".{target_path.name}.dex_tmp_{uuid.uuid4().hex[:6]}"
                            )
                            shutil.copy2(source_path, tmp_target)
                            os.replace(tmp_target, target_path)
                        elif source_path.is_dir():
                            target_path.mkdir(parents=True, exist_ok=True)
                elif fm.mutation_type == "deleted":
                    if target_path.exists():
                        if target_path.is_dir():
                            shutil.rmtree(target_path, ignore_errors=True)
                        else:
                            target_path.unlink(missing_ok=True)

        await asyncio.to_thread(_sync_fs)

    async def __aenter__(self) -> "Trial":
        return self

    async def __aexit__(self, exc_type, exc_val, exc_tb) -> None:
        if not self._committed and not self._rolled_back:
            await self.rollback()


class DryExecClient:
    """Client for dry-run execution and state delta tracking."""

    def __init__(
        self,
        ffi_module: Any = None,
    ):
        self._ffi = ffi_module if ffi_module is not None else _dry_exec_ffi

    async def execute_ephemeral_action(
        self,
        environment: Environment,
        action: Action,
        trigger_blocked_syscall: bool = False,
        request_to_trigger: Optional[Tuple[str, str, str]] = None,
    ) -> StateDelta:
        """Runs action within ephemeral boundary and returns computed StateDelta."""
        return await self._execute_internal(
            environment,
            action,
            trigger_blocked_syscall=trigger_blocked_syscall,
            request_to_trigger=request_to_trigger,
        )

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
                "Ensure native extension is built ('maturin build' or 'pip install .')."
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

        # 4. Credential masking sanitization
        if environment.credential_masks:
            raw_delta = _sanitize_credentials(raw_delta, environment.credential_masks)

        # 5. Transform raw FFI dictionary into type-safe StateDelta
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
                    deltas=[
                        ByteDelta(
                            offset=d["offset"],
                            original=bytes(d["original"]),
                            mutated=bytes(d["mutated"]),
                        )
                        for d in fm.get("deltas", [])
                    ],
                    is_deferred_execution_hazard=fm.get(
                        "is_deferred_execution_hazard", False
                    ),
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
            deferred_execution_hazards=raw_delta.get("deferred_execution_hazards", 0),
            total_bytes_mutated=raw_delta.get("total_bytes_mutated", 0),
            duration_nanos=raw_delta.get("duration_nanos", 0),
            stdout=bytes(raw_delta.get("stdout", b"")),
            stderr=bytes(raw_delta.get("stderr", b"")),
            exit_code=raw_delta.get("exit_code", 0),
            decision=parse_decision(raw_delta.get("decision")),
        )

    async def run_trial(
        self,
        environment: Environment,
        action: Action,
        workspace_dir: Optional[str] = None,
        trigger_blocked_syscall: bool = False,
        request_to_trigger: Optional[Tuple[str, str, str]] = None,
    ) -> Trial:
        """Executes an action within an ephemeral trial boundary with atomic commit/rollback.

        When workspace_dir is specified, an ephemeral clone of the workspace is created in a scratch
        directory. Action runs in the scratch directory. The target workspace remains unmodified
        until `await trial.commit()` is explicitly called.
        """
        scratch_dir: Optional[str] = None
        trial_env = environment

        if workspace_dir is not None:
            scratch_dir = tempfile.mkdtemp(prefix="dry_exec_trial_")
            ws_path = Path(workspace_dir).resolve()
            if ws_path.exists():
                await asyncio.to_thread(
                    shutil.copytree, str(ws_path), scratch_dir, dirs_exist_ok=True
                )

            trial_env = environment.model_copy(
                update={"allowed_filesystem_roots": [scratch_dir]}
            )

        delta = await self.execute_ephemeral_action(
            trial_env,
            action,
            trigger_blocked_syscall=trigger_blocked_syscall,
            request_to_trigger=request_to_trigger,
        )

        return Trial(
            delta=delta,
            environment=environment,
            action=action,
            scratch_dir=scratch_dir,
            workspace_dir=workspace_dir,
        )

    def trial(
        self,
        environment: Environment,
        action: Action,
        workspace_dir: Optional[str] = None,
        trigger_blocked_syscall: bool = False,
        request_to_trigger: Optional[Tuple[str, str, str]] = None,
    ):
        """Async context manager for an ephemeral trial."""

        class _TrialContextManager:
            def __init__(self, client: "DryExecClient"):
                self._client = client
                self._trial: Optional[Trial] = None

            async def __aenter__(self) -> Trial:
                self._trial = await self._client.run_trial(
                    environment=environment,
                    action=action,
                    workspace_dir=workspace_dir,
                    trigger_blocked_syscall=trigger_blocked_syscall,
                    request_to_trigger=request_to_trigger,
                )
                return self._trial

            async def __aexit__(self, exc_type, exc_val, exc_tb) -> None:
                if (
                    self._trial
                    and not self._trial.is_committed
                    and not self._trial.is_rolled_back
                ):
                    await self._trial.rollback()

        return _TrialContextManager(self)
