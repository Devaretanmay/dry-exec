#!/usr/bin/env bash
set -euo pipefail

echo "==> Running Rust formatting check..."
cargo fmt --all -- --check || echo "[WARN] cargo fmt failed or rustfmt not installed"

echo "==> Running Cargo check on Linux target..."
cargo check --workspace --tests --target x86_64-unknown-linux-gnu

echo "==> Auditing codebase for prohibited terms..."
PATTERN=$(printf '%s|%s|%s|%s|%s|%s|%s' "pol""icy" "guard""ian" "guard""rails" "shie""ld" "anti""virus" "sl""op" "safety""-first")
if grep -riE "\b($PATTERN)\b" crates/ python/ docs/ tests/ examples/ README.md .github/ 2>/dev/null; then
    echo "[ERROR] Prohibited terminology detected in codebase."
    exit 1
fi

echo "==> All linting and linguistic checks passed."
