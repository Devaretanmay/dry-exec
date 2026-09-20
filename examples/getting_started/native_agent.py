"""3-line autonomous execution loop with dex."""

import asyncio
from dex import Agent


async def main():
    # 1. Initialize autonomous loop with task and sane defaults
    agent = Agent(task="Migrate user accounts to active status")

    # 2. Execute ephemeral loop: propose -> dry-run -> evaluate -> commit
    result = await agent.run()
    print(f"Autonomous loop complete: success={result.success}, trials={result.trials_conducted}")


if __name__ == "__main__":
    asyncio.run(main())
