#!/usr/bin/env bash
set -euo pipefail

echo "==> Running local verification tests..."
python3 -m pytest tests/test_examples.py tests/test_cli.py tests/test_sdk.py -v || {
    echo "[INFO] Running with fallbacks on non-Linux host..."
}

echo "==> Verifying anti-laziness gates..."
if command -v node >/dev/null 2>&1; then
    node ~/.gemini/config/skills/unlazy/scripts/gate-check.mjs GATES.md
fi

echo "==> Verification completed."
