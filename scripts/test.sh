#!/usr/bin/env bash
set -euo pipefail

echo "==> Running Cargo tests..."
if [ "$(uname)" = "Darwin" ]; then
    RUSTFLAGS="-C link-arg=-undefined -C link-arg=dynamic_lookup" cargo test --workspace
else
    cargo test --workspace
fi

echo "==> Running Python verification tests..."
python3 -m pytest tests/ -v

echo "==> Verification completed successfully."
