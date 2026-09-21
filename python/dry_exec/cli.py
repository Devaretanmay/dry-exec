"""Developer CLI and autonomous execution loop runner for dry-exec (de)."""

import asyncio
import json
from pathlib import Path
import subprocess
from typing import List, Optional, Tuple
import uuid
import typer
import yaml
from rich.console import Console
from dry_exec.client import DryExecClient
from dry_exec.exceptions import DryExecError
from dry_exec.observability import DeltaLogger
from dry_exec.schemas import Action, Environment, MockResponse

app = typer.Typer(
    name="dex",
    help="Deterministic ephemeral execution primitive for autonomous execution loops.",
    no_args_is_help=True,
    add_completion=False,
)

console = Console()
logger = DeltaLogger(console)

# Explicit dex verbs resolved ahead of direct command execution.
_DEX_VERBS = {"run", "inspect", "version"}

# Option spellings recovered from the positional stream: click stops option parsing at the
# first positional token, so `dex "npm run seed" --commit` arrives with the flag unparsed.
_DIRECT_OPTION_FLAGS = {
    "--commit": "commit",
    "--config": "config",
    "-c": "config",
    "--action": "action",
    "-a": "action",
    "--net-request": "network_request",
}


def load_environment(config_path: Path) -> Environment:
    """Load and parse environment boundary specification from YAML or JSON."""
    if not config_path.exists():
        raise typer.BadParameter(f"Configuration file not found: {config_path}")

    with open(config_path, "r", encoding="utf-8") as f:
        if config_path.suffix in [".yaml", ".yml"]:
            data = yaml.safe_load(f)
        else:
            data = json.load(f)

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


def _execute_cli_flow(
    command: Optional[str] = None,
    config: Optional[Path] = None,
    action: Optional[Path] = None,
    auto_commit: bool = False,
    network_request: Optional[str] = None,
) -> None:
    """Core execution handler for dry-exec direct command and configured runs."""
    try:
        if config is not None:
            env = load_environment(config)
        else:
            env = Environment(
                name="cli_ephemeral_sandbox",
                allowed_mutation_targets={"*"},
                memory_limit_bytes=64 * 1024 * 1024,
            )

        if action is not None:
            act = load_action(action)
        else:
            target_cmd = command or "true"
            act = Action(
                action_id=f"cmd_{uuid.uuid4().hex[:6]}",
                target_resource="shell_command",
                mutation_type="execute",
                payload={"command": target_cmd},
            )
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
            confirmed = typer.confirm(
                "\nCommit state delta to target environment?", default=False
            )

        logger.render_commit_prompt(confirmed)
        if confirmed:
            if command:
                subprocess.run(command, shell=True, check=True)
        else:
            raise typer.Exit(code=0)

    except DryExecError as exc:
        logger.render_violation(exc)
        raise typer.Exit(code=2)
    except typer.Exit:
        raise
    except Exception as exc:
        console.print(f"[bold red]Execution Layer Error:[/bold red] {exc}")
        raise typer.Exit(code=3)


def _render_version() -> None:
    """Render dex (dry-exec) version and kernel primitive capabilities."""
    console.print("[bold green]dex[/bold green] (dry-exec) v1.0.0")
    console.print("Architecture: Ephemeral Kernel Isolation & State Delta Engine")
    console.print(
        "Primitives: Linux Namespaces, Seccomp-BPF TRAP, Soft-Dirty Pagemap, Transparent Network Proxy, macOS Seatbelt & APFS CoW"
    )


def _inspect_environment(config: Path) -> None:
    """Render environment boundary definitions, mutation whitelists, and mock endpoints."""
    try:
        env = load_environment(config)
        console.print(f"[bold cyan]Environment:[/bold cyan] {env.name}")
        console.print(
            f"[bold cyan]Allowed Mutation Targets:[/bold cyan] {sorted(list(env.allowed_mutation_targets))}"
        )
        console.print(
            f"[bold cyan]Filesystem Roots:[/bold cyan] {env.allowed_filesystem_roots}"
        )
        console.print(
            f"[bold cyan]Memory Limit:[/bold cyan] {env.memory_limit_bytes:,} bytes"
        )
        console.print(
            f"[bold cyan]Mock API Routes:[/bold cyan] {list(env.allowed_api_endpoints.keys())}"
        )
    except Exception as e:
        console.print(f"[bold red]Inspection Error:[/bold red] {e}")
        raise typer.Exit(code=1)


def _split_direct_options(
    tokens: List[str],
    commit: bool,
    config: Optional[Path],
    action: Optional[Path],
    network_request: Optional[str],
) -> Tuple[List[str], bool, Optional[Path], Optional[Path], Optional[str]]:
    """Separate control options from the positional token stream.

    `--` terminates option recovery so wrapped commands may carry their own flags.
    """
    positional: List[str] = []
    index = 0
    while index < len(tokens):
        token = tokens[index]
        if token == "--":
            positional.extend(tokens[index + 1 :])
            break

        target = _DIRECT_OPTION_FLAGS.get(token)
        if target == "commit":
            commit = True
            index += 1
            continue
        if target is not None:
            if index + 1 >= len(tokens):
                console.print(
                    f"[bold red]Configuration Error:[/bold red] {token} requires a value"
                )
                raise typer.Exit(code=1)
            value = tokens[index + 1]
            if target == "config":
                config = Path(value)
            elif target == "action":
                action = Path(value)
            else:
                network_request = value
            index += 2
            continue

        positional.append(token)
        index += 1

    return positional, commit, config, action, network_request


def _dispatch_verb(
    verb: str,
    command: Optional[str],
    config: Optional[Path],
    action: Optional[Path],
    auto_commit: bool,
    network_request: Optional[str],
) -> None:
    """Dispatch an explicit dex verb to its control flow handler."""
    if verb == "run":
        _execute_cli_flow(
            command=command,
            config=config,
            action=action,
            auto_commit=auto_commit,
            network_request=network_request,
        )
    elif verb == "inspect":
        if config is None:
            console.print("[bold red]Inspection Error:[/bold red] --config is required")
            raise typer.Exit(code=1)
        _inspect_environment(config)
    else:
        _render_version()


@app.callback(invoke_without_command=True)
def main_callback(
    ctx: typer.Context,
    tokens: Optional[List[str]] = typer.Argument(
        None,
        help="dex verb ('run', 'inspect', 'version') or command to execute within the boundary",
    ),
    commit: bool = typer.Option(
        False,
        "--commit",
        help="Commit state mutation to host environment after verification",
    ),
    config: Optional[Path] = typer.Option(
        None,
        "--config",
        "-c",
        help="Optional path to environment configuration (YAML/JSON)",
    ),
    action: Optional[Path] = typer.Option(
        None,
        "--action",
        "-a",
        help="Optional path to proposed action payload (YAML/JSON)",
    ),
    network_request: Optional[str] = typer.Option(
        None,
        "--net-request",
        help="Optional network request test: 'METHOD /path payload'",
    ),
) -> None:
    """Execute arbitrary commands or configured workflows in an ephemeral kernel boundary."""
    if ctx.invoked_subcommand is not None:
        return

    # A root-level positional argument is captured before click resolves subcommands, so dex
    # verbs and trailing control options are dispatched explicitly from the token stream.
    tokens, commit, config, action, network_request = _split_direct_options(
        list(tokens or []), commit, config, action, network_request
    )

    verb = tokens[0] if tokens else None
    if verb in _DEX_VERBS:
        _dispatch_verb(
            verb,
            command=" ".join(tokens[1:]) or None,
            config=config,
            action=action,
            auto_commit=commit,
            network_request=network_request,
        )
        return

    command = " ".join(tokens) if tokens else None

    if command is None and config is None and action is None:
        console.print(
            "[bold green]dex[/bold green] (dry-exec) - Ephemeral kernel execution primitive\n"
        )
        console.print("Usage: dex [OPTIONS] COMMAND")
        console.print("       dex run [--config ... --action ...]")
        console.print("       dex inspect --config ...\n")
        console.print("Examples:")
        console.print('  dex "python migrate.py"')
        console.print('  dex --commit "npm run seed"')
        console.print("  dex run --config env.yaml --action action.yaml\n")
        console.print("[dim]Aliases: 'de', 'dry-exec'[/dim]\n")
        return

    _execute_cli_flow(
        command=command,
        config=config,
        action=action,
        auto_commit=commit,
        network_request=network_request,
    )


@app.command(name="run")
def run(
    command: Optional[str] = typer.Argument(
        None,
        help="Command to execute within ephemeral isolation boundary",
    ),
    config: Optional[Path] = typer.Option(
        None,
        "--config",
        "-c",
        help="Path to environment configuration (YAML/JSON)",
    ),
    action: Optional[Path] = typer.Option(
        None,
        "--action",
        "-a",
        help="Path to proposed action payload (YAML/JSON)",
    ),
    auto_commit: bool = typer.Option(
        False,
        "--commit",
        help="Commit state mutation without interactive confirmation",
    ),
    network_request: Optional[str] = typer.Option(
        None,
        "--net-request",
        help="Optional network request test: 'METHOD /path payload'",
    ),
) -> None:
    """Execute a proposed action or command within an ephemeral isolation boundary."""
    _execute_cli_flow(
        command=command,
        config=config,
        action=action,
        auto_commit=auto_commit,
        network_request=network_request,
    )


@app.command()
def inspect(
    config: Path = typer.Option(
        ..., "--config", "-c", help="Path to environment configuration (YAML/JSON)"
    ),
) -> None:
    """Inspect environment boundary definitions, mutation whitelists, and mock endpoints."""
    _inspect_environment(config)


@app.command()
def version() -> None:
    """Display dex (dry-exec) version and kernel primitive capabilities."""
    _render_version()


def main() -> None:
    """CLI application entrypoint."""
    app()


if __name__ == "__main__":
    main()
