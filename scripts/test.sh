#!/usr/bin/env bash
set -euo pipefail

echo "==> Running Cargo tests..."
if [ "$(uname)" = "Darwin" ]; then
    RUSTFLAGS="-C link-arg=-undefined -C link-arg=dynamic_lookup" cargo test --workspace
else
    cargo test --workspace
fi

echo "==> Running Python verification tests..."
python3 -m pytest tests/test_dex_ergonomics.py tests/test_examples.py tests/test_telemetry_agent.py tests/test_sdk.py -v

echo "==> Verifying acceptance gates..."
if command -v node >/dev/null 2>&1; then
    node ~/.gemini/config/skills/unlazy/scripts/gate-check.mjs GATES.md
fi

echo "==> Verification completed successfully."
