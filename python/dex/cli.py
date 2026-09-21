"""Backward-compatibility shim for dry_exec.cli."""

from dry_exec.cli import (  # noqa: F401
    app,
    console,
    inspect,
    load_action,
    load_environment,
    logger,
    main,
    run,
    version,
)

__all__ = [
    "app",
    "main",
    "run",
    "inspect",
    "version",
    "load_environment",
    "load_action",
    "console",
    "logger",
]

if __name__ == "__main__":
    main()
