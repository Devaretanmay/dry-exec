#!/usr/bin/env bash
set -euo pipefail

echo "==> Building Rust workspace (core and pyo3 bridge)..."
cargo build --workspace --target x86_64-unknown-linux-gnu

if command -v maturin >/dev/null 2>&1; then
    echo "==> Building Python extension with maturin..."
    maturin develop
else
    echo "[INFO] maturin not found in PATH; skipping Python extension local compilation."
fi

echo "==> Build completed successfully."
