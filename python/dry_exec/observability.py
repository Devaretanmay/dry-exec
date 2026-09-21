"""Observability and state delta visualization module for dry-exec."""

from typing import Optional
from rich.box import ROUNDED
from rich.console import Console
from rich.panel import Panel
from rich.table import Table
from rich.text import Text
from dry_exec.decision import is_escalated
from dry_exec.exceptions import (
    DecisionEscalationError,
    SchemaViolationError,
    SyscallBoundaryError,
)
from dry_exec.models import DecisionReceipt, StateDelta
from dry_exec.schemas import Action, Environment
from dry_exec.telemetry import TelemetryExporter

# Guidance rendered whenever the System-One decision layer routes a state delta to escalation.
_FORCE_COMMIT_GUIDANCE = "Escalation required. Re-run with --commit to force."


class DeltaLogger:
    """Terminal visualization logger rendering state deltas and execution telemetry."""

    def __init__(
        self,
        console: Optional[Console] = None,
        telemetry_exporter: Optional[TelemetryExporter] = None,
        enable_json_telemetry: bool = False,
    ):
        self.console = console or Console()
        self.telemetry = telemetry_exporter or (
            TelemetryExporter(emit_json_logs=True) if enable_json_telemetry else None
        )

    def render_action_header(self, environment: Environment, action: Action) -> None:
        """Render proposed action and target environment boundary."""
        table = Table(box=ROUNDED, show_header=False, expand=True)
        table.add_column("Key", style="bold cyan", width=22)
        table.add_column("Value", style="white")

        table.add_row("Environment", environment.name)
        table.add_row("Action ID", action.action_id)
        table.add_row("Target Resource", action.target_resource)
        table.add_row("Mutation Type", action.mutation_type)
        table.add_row("Payload", str(action.payload))

        panel = Panel(
            table,
            title="[bold yellow]Ephemeral State Exploration: Proposed Action[/bold yellow]",
            subtitle="[dim]Pre-Execution Boundary Verification[/dim]",
            border_style="yellow",
        )
        self.console.print(panel)

    def render_delta(self, delta: StateDelta, trial_id: int = 1) -> None:
        """Render detailed memory, filesystem, and network state mutations."""
        # 1. Summary Metrics
        summary_table = Table(box=ROUNDED, show_header=True, expand=True)
        summary_table.add_column("Metric", style="bold green")
        summary_table.add_column("Count / Value", style="white")

        import sys

        if sys.platform == "darwin":
            summary_table.add_row(
                "Memory Pages Mutated",
                "N/A [dim yellow](macOS bare-metal; Linux pagemap only)[/dim yellow]",
            )
        else:
            summary_table.add_row(
                "Memory Pages Mutated", str(len(delta.memory_mutations))
            )
        summary_table.add_row("Filesystem Changes", str(len(delta.fs_mutations)))
        summary_table.add_row(
            "Network Requests Intercepted", str(len(delta.network_mutations))
        )
        summary_table.add_row(
            "Total Mutated Bytes", f"{delta.total_bytes_mutated:,} bytes"
        )
        scan_backend = (
            "APFS clonefile" if sys.platform == "darwin" else "kernel O(P_dirty) scan"
        )
        summary_table.add_row(
            "Computation Latency",
            f"{delta.duration_nanos / 1_000_000:.3f} ms ({scan_backend})",
        )

        self.console.print(
            Panel(
                summary_table,
                title=f"[bold green]State Delta Receipt (Trial #{trial_id})[/bold green]",
                border_style="green",
            )
        )
        if sys.platform == "darwin":
            self.console.print(
                "[dim]Note: Memory page tracking uses Linux /proc/[pid]/pagemap. "
                "Filesystem mutations (APFS CoW) and network proxy requests are fully tracked on macOS.[/dim]"
            )

        # 2. Memory Mutations Breakdown
        if delta.memory_mutations:
            mem_table = Table(box=ROUNDED, show_header=True, expand=True)
            mem_table.add_column("Page Index", style="cyan", width=12)
            mem_table.add_column("Virtual Address", style="magenta", width=18)
            mem_table.add_column("Offset", style="yellow", width=12)
            mem_table.add_column("Original Bytes", style="red")
            mem_table.add_column("Mutated Bytes", style="green")

            for pm in delta.memory_mutations:
                for d in pm.deltas:
                    mem_table.add_row(
                        str(pm.page_index),
                        f"0x{pm.page_address:08x}",
                        f"+{d.offset}",
                        d.original.hex(),
                        d.mutated.hex(),
                    )

            self.console.print(
                Panel(
                    mem_table,
                    title="[bold blue]Memory Mutations (Copy-on-Write Pages)[/bold blue]",
                    border_style="blue",
                )
            )

        # 3. Filesystem Mutations Breakdown
        if delta.fs_mutations:
            fs_table = Table(box=ROUNDED, show_header=True, expand=True)
            fs_table.add_column("Mutation Type", style="cyan", width=16)
            fs_table.add_column("Relative Path", style="white")
            fs_table.add_column("Size", style="yellow", width=12)

            for fm in delta.fs_mutations:
                fs_table.add_row(
                    fm.mutation_type.upper(),
                    fm.path,
                    f"{fm.size} B" if fm.size is not None else "-",
                )

            self.console.print(
                Panel(
                    fs_table,
                    title="[bold magenta]Ephemeral Filesystem Mutations (tmpfs Overlay)[/bold magenta]",
                    border_style="magenta",
                )
            )

        # 4. Network Interceptions Breakdown
        if delta.network_mutations:
            net_table = Table(box=ROUNDED, show_header=True, expand=True)
            net_table.add_column("Method", style="bold cyan", width=8)
            net_table.add_column("Target URL", style="white")
            net_table.add_column("Status", style="yellow", width=8)
            net_table.add_column("Outbound Payload", style="red")
            net_table.add_column("Deterministic Mock Response", style="green")

            for req in delta.network_mutations:
                req_preview = req.request_body.decode(errors="replace")[:60]
                resp_preview = req.response_body.decode(errors="replace")[:60]
                net_table.add_row(
                    req.method,
                    req.url,
                    str(req.response_status),
                    req_preview,
                    resp_preview,
                )

            self.console.print(
                Panel(
                    net_table,
                    title="[bold cyan]Transparent Network Proxy: Interceptions[/bold cyan]",
                    border_style="cyan",
                )
            )

    def render_decision(self, receipt: Optional[DecisionReceipt]) -> None:
        """Render the System-One decision receipt: categorical choice, calibrated score, and escalation state."""
        if receipt is None:
            return

        escalated = is_escalated(receipt)
        table = Table(box=ROUNDED, show_header=False, expand=True)
        table.add_column("Key", style="bold cyan", width=22)
        table.add_column("Value", style="white")

        table.add_row("Decision Choice", receipt.choice.value)
        table.add_row(
            "Risk Score",
            f"{receipt.risk_score:.2f} [dim](calibrated range 0.00 - 1.00)[/dim]",
        )
        table.add_row("Escalation", "Required" if escalated else "Auto-commit")
        table.add_row("System Reason", receipt.reason)
        if escalated:
            table.add_row("", f"[bold red]{_FORCE_COMMIT_GUIDANCE}[/bold red]")

        self.console.print(
            Panel(
                table,
                title=(
                    "[bold red]System-One Decision: Escalation Required[/bold red]"
                    if escalated
                    else "[bold green]System-One Decision Receipt[/bold green]"
                ),
                subtitle="[dim]Deterministic routing over calibrated scalar metrics[/dim]",
                border_style="red" if escalated else "green",
            )
        )

    def render_error(self, error: Exception, trial_id: int = 1) -> None:
        """Alias for render_violation with optional trial_id for multi-trial loops."""
        self.render_violation(error)

    def render_violation(self, error: Exception) -> None:
        """Render boundary violations with diagnostic telemetry for self-correction."""
        if self.telemetry is not None:
            # Emit structured telemetry log
            self.telemetry.logger.error(f"Execution boundary violation: {error}")
        if isinstance(error, SyscallBoundaryError):
            text = Text()
            text.append("Boundary Interception: Blocked Syscall\n", style="bold red")
            text.append(
                f"Offending Syscall Number: {error.syscall_nr}\n", style="yellow"
            )
            text.append(
                f"Instruction Pointer: 0x{error.instruction_pointer:08x}\n",
                style="cyan",
            )
            text.append(
                "Telemetry: The execution layer intercepted an unpermitted syscall. "
                "The autonomous execution loop should adjust its control flow.",
                style="dim",
            )
            self.console.print(
                Panel(
                    text,
                    title="[bold red]Kernel Boundary Violation[/bold red]",
                    border_style="red",
                )
            )
        elif isinstance(error, SchemaViolationError):
            text = Text()
            text.append("Pre-Execution Schema Rejection\n", style="bold red")
            text.append(f"Invalid Target: {error.invalid_target}\n", style="yellow")
            text.append(f"Violation Type: {error.violation_type}\n", style="cyan")
            text.append(
                "Gatekeeping: The proposed state mutation was rejected synchronously. "
                "Rust kernel FFI was never invoked.",
                style="dim",
            )
            self.console.print(
                Panel(
                    text,
                    title="[bold red]Schema Boundary Violation[/bold red]",
                    border_style="red",
                )
            )
        elif isinstance(error, DecisionEscalationError):
            text = Text()
            text.append("System-One Escalation Required\n", style="bold red")
            text.append(f"Decision Choice: {error.choice}\n", style="yellow")
            text.append(f"Risk Score: {error.risk_score:.2f}\n", style="yellow")
            text.append(f"System Reason: {error.reason}\n", style="cyan")
            text.append(
                f"{_FORCE_COMMIT_GUIDANCE} The state delta is not auto-committed.",
                style="dim",
            )
            self.console.print(
                Panel(
                    text,
                    title="[bold red]Decision Escalation[/bold red]",
                    border_style="red",
                )
            )
        else:
            self.console.print(
                Panel(
                    str(error),
                    title="[bold red]Execution Boundary Error[/bold red]",
                    border_style="red",
                )
            )

    def render_commit_prompt(self, confirmed: bool) -> None:
        """Render final human-in-the-loop state commitment decision."""
        if confirmed:
            self.console.print(
                Panel(
                    "[bold green]State Delta Confirmed: Applying state mutation to target environment.[/bold green]",
                    border_style="green",
                )
            )
        else:
            self.console.print(
                Panel(
                    "[bold yellow]State Mutation Discarded: Ephemeral boundary torn down with zero baseline changes.[/bold yellow]",
                    border_style="yellow",
                )
            )
