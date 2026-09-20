"""Zero-boilerplate quickstart with dex: 3 lines to ephemeral execution."""

import asyncio
import dex


@dex.dry_run
def update_system_status():
    return {"status": "online"}


async def main():
    delta = update_system_status()
    print(f"Ephemeral execution: {delta.total_bytes_mutated} bytes mutated in {delta.duration_nanos}ns")


if __name__ == "__main__":
    asyncio.run(main())
