"""Developer CLI and autonomous execution loop runner for dry-exec / dex."""

from dex.cli import (
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
