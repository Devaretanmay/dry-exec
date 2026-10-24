"""Async client for dry-exec ephemeral kernel isolation."""

import asyncio
import hashlib
import os
from pathlib import Path
import shutil
import sys
import tempfile
from typing import Any, Dict, Optional, Tuple
import uuid

from dry_exec.decision import enforce_escalation, parse_decision
from dry_exec.exceptions import (
    IsolationSetupError,
    SchemaViolationError,
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
from dry_exec.schemas import Action, Environment, SecurityLevel

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


async def _wait_for_completion(awaitable):
    """Keep native work alive until it is safe for its owner to clean up."""
    task = asyncio.ensure_future(awaitable)
    cancelled = False
    while not task.done():
        try:
            await asyncio.shield(task)
        except asyncio.CancelledError:
            cancelled = True
    result = task.result()
    if cancelled:
        raise asyncio.CancelledError
    return result


class Trial:
    """Ephemeral trial execution primitive supporting atomic commit and rollback."""

    def __init__(
        self,
        delta: StateDelta,
        environment: Environment,
        action: Action,
        scratch_dir: Optional[str] = None,
        workspace_dir: Optional[str] = None,
        ephemeral_dir: Optional[str] = None,
        trial_root: Optional[str] = None,
    ):
        self._delta = delta
        self._environment = environment
        self._action = action
        self._scratch_dir = scratch_dir
        self._workspace_dir = workspace_dir
        self._ephemeral_dir = ephemeral_dir
        self._trial_root = trial_root
        self._committed = False
        self._rolled_back = False
        self._lifecycle_lock = asyncio.Lock()

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
        async with self._lifecycle_lock:
            await _wait_for_completion(self._commit(force))

    async def _commit(self, force: bool) -> None:
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
        async with self._lifecycle_lock:
            if self._rolled_back or self._committed:
                return
            self._rolled_back = True
            self._cleanup_scratch()

    def _cleanup_scratch(self) -> None:
        if self._trial_root and os.path.exists(self._trial_root):
            shutil.rmtree(self._trial_root, ignore_errors=True)
            return
        for path in (self._scratch_dir, self._ephemeral_dir):
            if path and os.path.exists(path):
                shutil.rmtree(path, ignore_errors=True)

    @staticmethod
    def _relative_source(raw_path: str, scratch: Path) -> Optional[Path]:
        candidate = Path(raw_path)
        if candidate.is_absolute():
            # Resolve parent symlinks (for /tmp -> /private/tmp canonicalization) but keep the
            # final component lexical so a created symlink is preserved instead of followed.
            normalized = Path(os.path.realpath(candidate.parent)) / candidate.name
            try:
                rel = normalized.relative_to(scratch)
            except ValueError:
                return None
        else:
            rel = candidate
        rel = Path(os.path.normpath(str(rel)))
        if rel.is_absolute() or ".." in rel.parts or rel == Path("."):
            return None
        if rel.parts and rel.parts[0].startswith(".dex_"):
            return None
        return rel

    @staticmethod
    def _assert_inside_workspace(target: Path, workspace: Path) -> None:
        """Reject writes that would traverse a workspace symlink outside the target tree."""
        parent = target.parent.resolve()
        if os.path.commonpath([str(parent), str(workspace)]) != str(workspace):
            raise RuntimeError(
                f"Refusing to apply mutation outside workspace via symlink: {target}"
            )

    @staticmethod
    def _journal_name(target: Path) -> str:
        digest = hashlib.sha256()
        if target.is_file() and not target.is_symlink():
            with open(target, "rb") as handle:
                for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                    digest.update(chunk)
            return f"{digest.hexdigest()}-{uuid.uuid4().hex[:8]}"
        return f"dir-{uuid.uuid4().hex}"

    @staticmethod
    def _copy_file_durable(source: Path, target: Path) -> None:
        with open(source, "rb") as src, open(target, "wb") as dst:
            shutil.copyfileobj(src, dst)
            dst.flush()
            os.fsync(dst.fileno())
        shutil.copystat(source, target)

    @staticmethod
    def _fsync_directory(path: Path) -> None:
        try:
            fd = os.open(str(path), os.O_RDONLY)
        except OSError:
            return
        try:
            os.fsync(fd)
        except OSError:
            pass
        finally:
            os.close(fd)

    async def _apply_mutations_to_workspace(self) -> None:
        if not self._workspace_dir or not self._scratch_dir:
            return
        await asyncio.to_thread(self._apply_mutations_sync)

    def _apply_mutations_sync(self) -> None:
        ws = Path(self._workspace_dir).resolve()
        sc = Path(self._scratch_dir).resolve()

        operations: Dict[Path, str] = {}
        for mutation in self._delta.fs_mutations:
            rel = self._relative_source(mutation.path, sc)
            if rel is not None:
                operations[rel] = mutation.mutation_type

        if not operations:
            return

        journal = Path(tempfile.mkdtemp(prefix=".dex_journal_", dir=str(ws)))
        stage = Path(tempfile.mkdtemp(prefix=".dex_stage_", dir=str(ws)))
        backups: Dict[Path, Path] = {}
        applied: list[Path] = []
        created_dirs: list[Path] = []
        preserve_journal = False

        try:
            for rel in sorted(operations):
                mutation = operations[rel]
                target = ws / rel
                source = sc / rel

                if os.path.commonpath([os.path.abspath(target), str(ws)]) != str(ws):
                    raise RuntimeError(
                        f"Refusing to apply mutation outside workspace: {rel}"
                    )
                self._assert_inside_workspace(target, ws)

                if mutation in ("created", "modified") and not os.path.lexists(source):
                    continue

                if os.path.lexists(target):
                    backup = journal / self._journal_name(target)
                    os.replace(target, backup)
                    backups[target] = backup

                if mutation not in ("created", "modified"):
                    continue

                missing: list[Path] = []
                parent = target.parent
                while parent != ws and not os.path.lexists(parent):
                    missing.append(parent)
                    parent = parent.parent
                target.parent.mkdir(parents=True, exist_ok=True)
                created_dirs.extend(reversed(missing))

                if source.is_symlink():
                    staged = stage / uuid.uuid4().hex
                    os.symlink(os.readlink(source), staged)
                    os.replace(staged, target)
                    applied.append(target)
                elif source.is_dir():
                    target.mkdir(parents=True, exist_ok=True)
                    applied.append(target)
                elif source.is_file():
                    staged = stage / uuid.uuid4().hex
                    self._copy_file_durable(source, staged)
                    os.replace(staged, target)
                    applied.append(target)

            for directory in {path.parent for path in applied} | {
                path.parent for path in backups
            }:
                self._fsync_directory(directory)
        except BaseException as error:
            recovery_errors = []
            for target in reversed(applied):
                try:
                    if target.is_dir() and not target.is_symlink():
                        shutil.rmtree(target, ignore_errors=True)
                    else:
                        target.unlink(missing_ok=True)
                except OSError as recovery_error:
                    recovery_errors.append(recovery_error)
            for target, backup in backups.items():
                try:
                    target.parent.mkdir(parents=True, exist_ok=True)
                    os.replace(backup, target)
                except OSError as recovery_error:
                    recovery_errors.append(recovery_error)
            for directory in reversed(created_dirs):
                try:
                    directory.rmdir()
                except OSError:
                    pass
            if recovery_errors:
                preserve_journal = True
                raise RuntimeError(
                    f"Commit failed and recovery was incomplete. Original files are retained "
                    f"in {journal}. Recovery error: {recovery_errors[0]}"
                ) from error
            raise
        finally:
            if not preserve_journal:
                shutil.rmtree(journal, ignore_errors=True)
            shutil.rmtree(stage, ignore_errors=True)

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

    def _available_security_level(self) -> SecurityLevel:
        """Isolation strength the current native backend actually provides."""
        if sys.platform not in ("linux", "darwin"):
            return SecurityLevel.NONE

        capabilities = getattr(self._ffi, "isolation_capabilities", None)
        if capabilities is not None:
            try:
                landlock, userns = capabilities()
            except Exception:
                landlock = userns = False
            if landlock and userns:
                return SecurityLevel.USERNS_LANDLOCK

        return SecurityLevel.PROCESS

    def _assert_security_level(self, environment: Environment) -> None:
        required = environment.minimum_security_level
        available = self._available_security_level()
        levels = list(SecurityLevel)
        if levels.index(required) > levels.index(available):
            raise IsolationSetupError(
                f"Environment '{environment.name}' requires {required.value} isolation, "
                f"but this backend provides only {available.value}. Refusing to run rather "
                "than silently degrading the boundary."
            )

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
        workspace_root: Optional[str] = None,
    ) -> StateDelta:
        # 1. Synchronous schema validation gatekeeping (pre-execution boundary)
        self._assert_security_level(environment)
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
            raw_delta: Dict[str, Any] = await _wait_for_completion(
                asyncio.to_thread(
                    self._ffi.execute_isolated_action,
                    action_payload_json,
                    environment.memory_limit_bytes,
                    tmpfs_path,
                    trigger_blocked_syscall,
                    mock_endpoints_tuples,
                    request_to_trigger,
                    decision_thresholds,
                    command,
                    sorted(environment.allowed_mutation_targets),
                    workspace_root,
                    environment.minimum_security_level == SecurityLevel.USERNS_LANDLOCK,
                )
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
            elif exc_type == "SchemaViolationError":
                raise SchemaViolationError(
                    str(exc),
                    invalid_target=getattr(exc, "invalid_target", ""),
                    violation_type=getattr(
                        exc, "violation_type", "native_schema_boundary"
                    ),
                ) from exc
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

    async def _clone_workspace(self, source: str, destination: str) -> None:
        clone_fn = getattr(self._ffi, "clone_directory", None) if self._ffi else None
        if clone_fn is not None:
            try:
                await _wait_for_completion(
                    asyncio.to_thread(clone_fn, source, destination)
                )
                return
            except Exception:
                pass
        await _wait_for_completion(
            asyncio.to_thread(
                shutil.copytree,
                source,
                destination,
                dirs_exist_ok=True,
                symlinks=True,
            )
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

        When workspace_dir is specified, a copy-on-write clone of the workspace is created in a
        scratch directory. The action runs with the scratch clone as its working directory, while
        a separate empty directory backs the ephemeral tmpfs. The target workspace remains
        unmodified until `await trial.commit()` is explicitly called.
        """
        trial_root = tempfile.mkdtemp(prefix="dry_exec_trial_")
        scratch_dir = os.path.join(trial_root, "workspace")
        ephemeral_dir = os.path.join(trial_root, "ephemeral")

        try:
            os.makedirs(ephemeral_dir, exist_ok=True)
            if workspace_dir is not None:
                ws_path = Path(workspace_dir).resolve()
                workspace_dir = str(ws_path)
                if not ws_path.is_dir():
                    raise FileNotFoundError(
                        f"workspace_dir does not exist or is not a directory: {workspace_dir}"
                    )
                await self._clone_workspace(str(ws_path), scratch_dir)
            else:
                os.makedirs(scratch_dir, exist_ok=True)

            trial_env = environment.model_copy(
                update={"allowed_filesystem_roots": [ephemeral_dir]}
            )
            delta = await self._execute_internal(
                trial_env,
                action,
                trigger_blocked_syscall=trigger_blocked_syscall,
                request_to_trigger=request_to_trigger,
                workspace_root=scratch_dir,
            )
        except BaseException:
            shutil.rmtree(trial_root, ignore_errors=True)
            raise

        return Trial(
            delta=delta,
            environment=environment,
            action=action,
            scratch_dir=scratch_dir,
            workspace_dir=workspace_dir,
            ephemeral_dir=ephemeral_dir,
            trial_root=trial_root,
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
