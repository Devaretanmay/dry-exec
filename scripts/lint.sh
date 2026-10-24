#!/usr/bin/env bash
set -euo pipefail

echo "==> Running Rust formatting check..."
cargo fmt --all -- --check

echo "==> Running Cargo check..."
if [ "$(uname)" = "Darwin" ]; then
    cargo check --workspace --tests
else
    cargo check --workspace --tests --target x86_64-unknown-linux-gnu
fi

echo "==> Auditing codebase for prohibited terms..."
PATTERN=$(printf '%s|%s|%s|%s|%s|%s|%s' "pol""icy" "guard""ian" "guard""rails" "shie""ld" "anti""virus" "sl""op" "safety""-first")
if grep -riE --binary-files=without-match "\b($PATTERN)\b" crates/ python/ docs/ tests/ examples/ README.md .github/ 2>/dev/null | grep -v 'badge.svg'; then
    echo "[ERROR] Prohibited terminology detected in codebase."
    exit 1
fi

echo "==> All linting and linguistic checks passed."
