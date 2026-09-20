"""Developer CLI and autonomous execution loop runner for dry-exec."""

import asyncio
import json
from pathlib import Path
from typing import Optional
import typer
import yaml
from rich.console import Console
from dry_exec.client import DryExecClient
from dry_exec.exceptions import DryExecError
from dry_exec.observability import DeltaLogger
from dry_exec.schemas import Action, Environment, MockResponse

app = typer.Typer(
    name="dry-exec",
    help="Deterministic ephemeral execution primitive for autonomous execution loops.",
    add_completion=False,
)
console = Console()
logger = DeltaLogger(console)


def load_environment(config_path: Path) -> Environment:
    """Load and parse environment boundary specification from YAML or JSON."""
    if not config_path.exists():
        raise typer.BadParameter(f"Configuration file not found: {config_path}")

    with open(config_path, "r", encoding="utf-8") as f:
        if config_path.suffix in [".yaml", ".yml"]:
            data = yaml.safe_load(f)
        else:
            data = json.load(f)

    # Process allowed_api_endpoints if present in dict form
    if "allowed_api_endpoints" in data:
        endpoints = {}
        for route, resp_data in data["allowed_api_endpoints"].items():
            if isinstance(resp_data, dict):
                endpoints[route] = MockResponse(**resp_data)
            else:
                endpoints[route] = MockResponse(body=str(resp_data))
        data["allowed_api_endpoints"] = endpoints

    return Environment(**data)


def load_action(action_path: Path) -> Action:
    """Load and parse proposed state-mutation action from YAML or JSON."""
    if not action_path.exists():
        raise typer.BadParameter(f"Action file not found: {action_path}")

    with open(action_path, "r", encoding="utf-8") as f:
        if action_path.suffix in [".yaml", ".yml"]:
            data = yaml.safe_load(f)
        else:
            data = json.load(f)

    return Action(**data)


@app.command()
def run(
    config: Path = typer.Option(..., "--config", "-c", help="Path to environment configuration (YAML/JSON)"),
    action: Path = typer.Option(..., "--action", "-a", help="Path to proposed action payload (YAML/JSON)"),
    auto_commit: bool = typer.Option(False, "--commit", help="Commit state mutation without interactive confirmation"),
    network_request: Optional[str] = typer.Option(
        None,
        "--net-request",
        help="Optional network request test: 'METHOD /path payload'",
    ),
) -> None:
    """Execute a proposed action within an ephemeral isolation boundary and inspect state deltas."""
    try:
        env = load_environment(config)
        act = load_action(action)
    except Exception as e:
        console.print(f"[bold red]Configuration Error:[/bold red] {e}")
        raise typer.Exit(code=1)

    logger.render_action_header(env, act)

    req_tuple = None
    if network_request:
        parts = network_request.split(maxsplit=2)
        if len(parts) >= 2:
            method = parts[0].upper()
            url = parts[1]
            body = parts[2] if len(parts) > 2 else ""
            req_tuple = (method, url, body)

    client = DryExecClient()

    async def _execute():
        return await client.execute_ephemeral_action(
            env, act, request_to_trigger=req_tuple
        )

    try:
        delta = asyncio.run(_execute())
        logger.render_delta(delta)

        # Human-in-the-loop control flow checkpoint
        if auto_commit:
            confirmed = True
        else:
            confirmed = typer.confirm("\nCommit state delta to target environment?", default=False)

        logger.render_commit_prompt(confirmed)
        if not confirmed:
            raise typer.Exit(code=0)

    except DryExecError as exc:
        logger.render_violation(exc)
        raise typer.Exit(code=2)
    except Exception as exc:
        console.print(f"[bold red]Execution Layer Error:[/bold red] {exc}")
        raise typer.Exit(code=3)


@app.command()
def inspect(
    config: Path = typer.Option(..., "--config", "-c", help="Path to environment configuration (YAML/JSON)"),
) -> None:
    """Inspect environment boundary definitions, mutation whitelists, and mock endpoints."""
    try:
        env = load_environment(config)
        console.print(f"[bold cyan]Environment:[/bold cyan] {env.name}")
        console.print(f"[bold cyan]Allowed Mutation Targets:[/bold cyan] {sorted(list(env.allowed_mutation_targets))}")
        console.print(f"[bold cyan]Filesystem Roots:[/bold cyan] {env.allowed_filesystem_roots}")
        console.print(f"[bold cyan]Memory Limit:[/bold cyan] {env.memory_limit_bytes:,} bytes")
        console.print(f"[bold cyan]Mock API Routes:[/bold cyan] {list(env.allowed_api_endpoints.keys())}")
    except Exception as e:
        console.print(f"[bold red]Inspection Error:[/bold red] {e}")
        raise typer.Exit(code=1)


@app.command()
def version() -> None:
    """Display dry-exec version and kernel primitive capabilities."""
    console.print("[bold green]dry-exec[/bold green] v1.0.0")
    console.print("Architecture: Ephemeral Kernel Isolation & State Delta Engine")
    console.print("Primitives: Linux Namespaces, Seccomp-BPF TRAP, Soft-Dirty Pagemap, Transparent Network Proxy")


def main() -> None:
    """CLI application entrypoint."""
    app()


if __name__ == "__main__":
    main()
