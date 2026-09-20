#!/usr/bin/env bash
set -euo pipefail

echo "==> Building Rust workspace..."
if [ "$(uname)" = "Darwin" ]; then
    echo "==> Detected macOS host: building native Darwin target..."
    RUSTFLAGS="-C link-arg=-undefined -C link-arg=dynamic_lookup" cargo build --workspace
    cargo check --workspace --target x86_64-unknown-linux-gnu
else
    cargo build --workspace
fi

if command -v maturin >/dev/null 2>&1; then
    echo "==> Building Python extension with maturin..."
    maturin build --target-dir target/maturin
fi

echo "==> Build completed successfully."
